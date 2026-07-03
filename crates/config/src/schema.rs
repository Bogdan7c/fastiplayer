use std::fmt;

use serde::{
    Deserialize, Deserializer, Serialize,
    de::{self, Visitor},
};

use crate::{ConfigResult, frame_server::FrameServerConfig, validation};

/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 4;

/// Старая схема до публичного выбора `auto`/`hardware`/`software`.
pub(crate) const LEGACY_SCHEMA_VERSION_2: u32 = 2;

/// Старая схема с уже удалённой галкой `video.hardware_decode_only`.
pub(crate) const LEGACY_SCHEMA_VERSION_3: u32 = 3;

/// Полная пользовательская конфигурация приложения.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(deny_unknown_fields)]
pub struct AppConfig {
    /// Версия TOML-схемы; обязательна для будущих миграций config.
    #[setting(
        id = "schema_version",
        path = "schema_version",
        section = "system",
        group = "schema",
        surface = "main-settings-window",
        label_id = "settings.system.schema_version.label",
        label_ru = "Версия схемы",
        description_id = "settings.system.schema_version.description",
        description_ru = "Read-only версия TOML-схемы для будущих миграций.",
        editor = "read_only",
        apply = "system.apply",
        read_only,
        default = "no_reset"
    )]
    pub schema_version: u32,

    /// Поведение playback state machine и выбора потоков.
    #[serde(default)]
    #[setting(nested)]
    pub player: PlayerConfig,

    /// Decode-ограничения и backend preference.
    #[serde(default)]
    #[setting(nested)]
    pub video: VideoConfig,

    /// Persisted TOML-настройки будущего Frame Server слоя.
    #[serde(default)]
    #[setting(nested)]
    pub frame_server: FrameServerConfig,

    /// Render-профиль и backend-specific настройки.
    #[serde(default)]
    #[setting(nested)]
    pub render: RenderConfig,

    /// Настройки аудиовыхода.
    #[serde(default)]
    #[setting(nested)]
    pub audio: AudioConfig,

    /// Настройки сетевого read-ahead/cache слоя.
    #[serde(default)]
    #[setting(nested)]
    pub network: NetworkConfig,

    /// Настройки YouTube/service слоя.
    #[serde(default)]
    #[setting(nested)]
    pub youtube: YoutubeConfig,

    /// Настройки shell UI.
    #[serde(default)]
    #[setting(nested)]
    pub ui: UiConfig,
}

impl AppConfig {
    /// Проверяет значения, которые Serde не может выразить типами.
    pub fn validate(&self) -> ConfigResult<()> {
        validation::validate_app_config(self)
    }

    /// Сериализует config в читаемый generated TOML для записи user config-файла.
    pub fn to_pretty_toml(&self) -> ConfigResult<String> {
        let mut toml_text = toml::to_string_pretty(self)
            .map_err(|source| crate::ConfigError::SerializeDefaultConfig { source })?;

        if !toml_text.ends_with('\n') {
            toml_text.push('\n');
        }
        document_schema_version_4_defaults(&mut toml_text);

        Ok(toml_text)
    }
}

/// Добавляет русские комментарии к полям schema version 4 в default TOML.
fn document_schema_version_4_defaults(toml_text: &mut String) {
    insert_default_config_comment(
        toml_text,
        "[player.seek]",
        "# Настройки seek commit, resume после seek и hotkey-шагов.",
    );
    insert_default_config_comment(
        toml_text,
        "commit_timeout_ms = 10000",
        "# Timeout финального seek/scrub commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_audio_min_buffer_ms = 50",
        "# Минимальный audio buffer перед resume после commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_audio_gate_timeout_ms = 250",
        "# Soft timeout audio gate-а после показанного target video frame.",
    );
    insert_default_config_comment(
        toml_text,
        "resume_video_min_ready_frames = 3",
        "# Минимальный запас готовых video frames перед resume после commit-а.",
    );
    insert_default_config_comment(
        toml_text,
        "fast_preroll_budget_ms = 48",
        "# Bounded окно worker work для accurate seek decode-preroll до target frame.",
    );
    insert_default_config_comment(
        toml_text,
        "fast_preroll_video_packet_burst = 512",
        "# Burst-лимит video packets/frames для accurate seek GOP preroll.",
    );
    insert_default_config_comment(
        toml_text,
        "paused_commit_behavior = \"stay_paused\"",
        "# Поведение seek commit-а, начатого из paused состояния.",
    );
    insert_default_config_comment(
        toml_text,
        "hotkey_small_step_secs = 5",
        "# Малый шаг seek hotkey в секундах.",
    );
    insert_default_config_comment(
        toml_text,
        "hotkey_large_step_secs = 30",
        "# Большой шаг seek hotkey в секундах.",
    );
    insert_default_config_comment(
        toml_text,
        "[player.demux]",
        "# Fail-safe настройки demuxer-а.",
    );
    insert_default_config_comment(
        toml_text,
        "max_consecutive_corrupted_packets = 64",
        "# Сколько corrupted packets подряд можно пропустить до fatal ошибки.",
    );
    insert_default_config_comment(
        toml_text,
        "[network]",
        "# Настройки будущего source/network cache слоя.",
    );
    insert_default_config_comment(
        toml_text,
        "memory_cache_mb = 128",
        "# RAM cache budget; 0 явно отключает RAM cache.",
    );
    insert_default_config_comment(
        toml_text,
        "read_ahead_mb = 256",
        "# RAM window, которое prefetch держит впереди foreground cursor-а.",
    );
    insert_default_config_comment(
        toml_text,
        "prefetch_chunk_mb = 8",
        "# Максимальный размер одного фонового prefetch-чтения.",
    );
    insert_default_config_comment(
        toml_text,
        "prefetch_initial_chunk_kb = 64",
        "# Размер ПЕРВОГО prefetch-чтения (slow-start), КиБ.",
    );
    insert_default_config_comment(
        toml_text,
        "connect_timeout_ms = 15000",
        "# Timeout подключения к сетевому источнику.",
    );
    insert_default_config_comment(
        toml_text,
        "read_timeout_ms = 15000",
        "# Timeout чтения из сетевого источника.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_packet_channel_frames = 32",
        "# Bounded очередь packets между worker и decoder thread.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_frame_channel_frames = 8",
        "# Bounded очередь decoded frames между decoder thread и worker.",
    );
    insert_default_config_comment(
        toml_text,
        "[frame_server]",
        "# Сохраняемые metadata-ready настройки будущего Frame Server; runtime-подключение появится позже.",
    );
    insert_default_config_comment(
        toml_text,
        "hover_preview_enabled = true",
        "# Включает только визуальный HoverPreview; текущий-target prepare не выключается config-ом.",
    );
    insert_default_config_comment(
        toml_text,
        "hover_pool_frames = \"auto\"",
        "# Бюджет кадрового пула hover: \"auto\" или положительное число без статического верхнего лимита.",
    );
    insert_default_config_comment(
        toml_text,
        "hover_thread_count = \"auto\"",
        "# Бюджет потоков hover: \"auto\" или положительное число без искусственного provider-лимита.",
    );
    insert_default_config_comment(
        toml_text,
        "hover_prepare_window_slots = 1",
        "# Основные слоты hover prepare window; допустимо 1..=3.",
    );
    insert_default_config_comment(
        toml_text,
        "software_hover_prepare_window_slots = 1",
        "# Основные слоты software hover prepare window; допустимо 1..=2.",
    );
    insert_default_config_comment(
        toml_text,
        "recent_superseded_prepare_slots = 1",
        "# Удержание недавно заменённых целей для быстрого возврата; 0 отключает только этот retention path.",
    );
    insert_default_config_comment(
        toml_text,
        "software_recent_superseded_prepare_slots = 1",
        "# Удержание недавно заменённых software-целей для быстрого возврата; 0 отключает только этот retention path.",
    );
    insert_default_config_comment(
        toml_text,
        "hover_leave_grace_ms = 500",
        "# UX grace после ухода hover; это не decode coverage budget.",
    );
    insert_default_config_comment(
        toml_text,
        "network_hover_prepare_throttle_ms = 300",
        "# Межстартовый throttle для network hover prepare; 0 убирает только delay.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_enabled = true",
        "# Включает live drag preview updates; точный seek по click/release остаётся активным.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_decode_mode = \"throttled_latest\"",
        "# Политика live scrub: throttled_latest или every_drag_event, оба latest-only.",
    );
    insert_default_config_comment(
        toml_text,
        "live_scrub_max_hz = 60",
        "# Максимальная частота live scrub decode-work для throttled_latest; допустимо 1..=240.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_ready_queue_frames = 8",
        "# Backend-local ready queue для burst FrameReady events.",
    );
    insert_default_config_comment(
        toml_text,
        "decoder_surface_pool_frames = 24",
        "# VA output surface descriptors для hardware decoder-а.",
    );
    insert_default_config_comment(
        toml_text,
        "sw_decoder_surface_pool_frames = 8",
        "# Сколько software (FFmpeg host RAM) кадров держать одновременно; меньше = стабильнее FPS на 4K.",
    );
    insert_default_config_comment(
        toml_text,
        "sw_decode_threads = 0",
        "# Потоки software-декода; 0 = авто (ядра − 2, чтобы render-поток не голодал).",
    );
    insert_default_config_comment(
        toml_text,
        "zero_copy_surface_pool_slots = 24",
        "# Zero-copy external import slots; CPU fallback всё равно запрещён.",
    );
    insert_default_config_comment(
        toml_text,
        "[video.scheduler]",
        "# Настройки worker scheduler-а для bounded catch-up после latency spike.",
    );
    insert_default_config_comment(
        toml_text,
        "demux_packets_per_tick = 12",
        "# Базовый budget чтения container packets за один worker tick.",
    );
    insert_default_config_comment(
        toml_text,
        "video_packets_per_tick = 8",
        "# Базовый budget отправки video packets в decoder thread за tick.",
    );
    insert_default_config_comment(
        toml_text,
        "decoded_frames_per_tick = 8",
        "# Базовый budget приёма decoded frames из decoder thread за tick.",
    );
    insert_default_config_comment(
        toml_text,
        "catch_up_budget_ms = 4",
        "# Дополнительное bounded окно catch-up work после обычного tick.",
    );
    insert_default_config_comment(
        toml_text,
        "present_queue_min_frames = 2",
        "# Минимальный запас ready frames, ниже которого diagnostics считает starvation.",
    );
    insert_default_config_comment(
        toml_text,
        "present_queue_target_frames = 4",
        "# Целевой запас ready frames; максимум задаёт video.present_queue_frames.",
    );
    insert_default_config_comment(
        toml_text,
        "decode_ahead_target_ms = 250",
        "# Целевой video decode-ahead; максимум задаёт video.max_decode_ahead_ms.",
    );
    insert_default_config_comment(
        toml_text,
        "surface_free_slots_min = 2",
        "# Минимальный резерв свободных zero-copy surface/import slots перед decode.",
    );
    insert_default_config_comment(
        toml_text,
        "surface_free_slots_target = 4",
        "# Целевой резерв surface/import slots для adaptive catch-up.",
    );
    insert_default_config_comment(
        toml_text,
        "[youtube]",
        "# Настройки YouTube/service adapter-а.",
    );
    insert_default_config_comment(
        toml_text,
        "resolve_timeout_ms = 30000",
        "# Timeout подготовки YouTube metadata через yt-dlp.",
    );
    insert_default_config_comment(
        toml_text,
        "skin = \"minimal\"",
        "# UI skin id; unknown id является config error.",
    );
    insert_default_config_comment(
        toml_text,
        "[ui.window]",
        "# Настройки кастомного заголовка окна Rustiplayer.",
    );
    insert_default_config_comment(
        toml_text,
        "titlebar_height_px = 40",
        "# Высота кастомного titlebar в логических UI px.",
    );
    insert_default_config_comment(
        toml_text,
        "[ui.settings]",
        "# Настройки поведения окна Settings UI.",
    );
    insert_default_config_comment(
        toml_text,
        "live_preview_max_hz = 60",
        "# Максимальная частота live preview updates в Settings UI.",
    );
    insert_default_config_comment(toml_text, "[ui.animations]", "# Настройки UI-анимаций.");
    insert_default_config_comment(
        toml_text,
        "sidebar_slide_duration_ms = 500",
        "# Длительность выезда settings sidebar и сжатия видео, мс; 0 отключает анимацию.",
    );
}

/// Вставляет комментарий перед ожидаемой строкой default TOML, не дублируя его.
fn insert_default_config_comment(toml_text: &mut String, needle: &str, comment: &str) {
    if toml_text.contains(comment) {
        return;
    }

    *toml_text = toml_text.replacen(needle, &format!("{comment}\n{needle}"), 1);
}

impl Default for AppConfig {
    /// Возвращает production defaults для первого запуска без config-файла.
    fn default() -> Self {
        Self {
            schema_version: CURRENT_SCHEMA_VERSION,
            player: PlayerConfig::default(),
            video: VideoConfig::default(),
            frame_server: FrameServerConfig::default(),
            render: RenderConfig::default(),
            audio: AudioConfig::default(),
            network: NetworkConfig::default(),
            youtube: YoutubeConfig::default(),
            ui: UiConfig::default(),
        }
    }
}

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
        label_ru = "Запускать на паузе",
        description_id = "settings.player.start_paused.description",
        description_ru = "Новое media открывается в paused состоянии.",
        editor = "toggle",
        apply = "player.apply"
    )]
    pub start_paused: bool,

    /// Зарезервировано для будущего восстановления позиции.
    #[setting(
        id = "player.resume_last_position",
        path = "player.resume_last_position",
        section = "player",
        group = "playback",
        surface = "main-settings-window",
        label_id = "settings.player.resume_last_position.label",
        label_ru = "Восстанавливать позицию",
        description_id = "settings.player.resume_last_position.description",
        description_ru = "Persistent policy для будущего восстановления последней позиции.",
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
        label_ru = "Приоритет видеокодеков",
        description_id = "settings.player.preferred_video_codec_order.description",
        description_ru = "Упорядоченный список codec ids для выбора video stream.",
        help_id = "settings.player.preferred_video_codec_order.help",
        help_ru = "Каждый codec id должен встречаться не больше одного раза; первый поддержанный вариант имеет больший приоритет.",
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
    /// Возвращает безопасное поведение первого запуска.
    fn default() -> Self {
        Self {
            start_paused: true,
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
        label_ru = "Лимит повреждённых packets",
        description_id = "settings.player.demux.max_consecutive_corrupted_packets.description",
        description_ru = "Сколько corrupted packets подряд можно пропустить до fatal ошибки.",
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
        description_ru = "Максимальное время финального seek/scrub commit-а.",
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
        label_ru = "Минимальный audio buffer",
        description_id = "settings.player.seek.resume_audio_min_buffer_ms.description",
        description_ru = "Минимальный audio buffer перед resume после seek commit.",
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
        description_ru = "Soft timeout audio gate-а после показа target video frame.",
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
        label_ru = "Готовые video frames перед resume",
        description_id = "settings.player.seek.resume_video_min_ready_frames.description",
        description_ru = "Минимальный запас decoded frames перед resume после seek commit.",
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
        description_ru = "Bounded окно worker work для accurate seek decode-preroll.",
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
        label_ru = "Burst packets для preroll",
        description_id = "settings.player.seek.fast_preroll_video_packet_burst.description",
        description_ru = "Лимит video packets/frames для accurate seek GOP preroll.",
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
        label_ru = "Поведение seek из паузы",
        description_id = "settings.player.seek.paused_commit_behavior.description",
        description_ru = "Что делать после seek commit, начатого из paused состояния.",
        editor = "select",
        apply = "player.apply",
        options(
            option(id = "stay_paused", label_id = "settings.player.seek.paused_commit_behavior.stay_paused", label_ru = "Оставаться на паузе", value = PausedCommitBehavior::StayPaused),
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
        label_ru = "Малый шаг seek",
        description_id = "settings.player.seek.hotkey_small_step_secs.description",
        description_ru = "Малый шаг seek hotkey в секундах.",
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
        label_ru = "Большой шаг seek",
        description_id = "settings.player.seek.hotkey_large_step_secs.description",
        description_ru = "Большой шаг seek hotkey в секундах.",
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
                 чтобы Rustiplayer выбрал поддерживаемый backend, или на \"hardware\", чтобы \
                 явно требовать native hardware decode",
            )),
            unknown_value => Err(E::unknown_variant(
                unknown_value,
                SUPPORTED_VIDEO_BACKEND_PREFERENCE_VALUES,
            )),
        }
    }
}

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
            resolve_timeout_ms: 30_000,
        }
    }
}

/// Настройки UI shell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Показывать диагностическую панель telemetry.
    #[setting(
        id = "ui.show_telemetry",
        path = "ui.show_telemetry",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.show_telemetry.label",
        label_ru = "Показывать telemetry",
        description_id = "settings.ui.show_telemetry.description",
        description_ru = "Показывать диагностическую панель telemetry.",
        editor = "toggle",
        apply = "ui.apply"
    )]
    pub show_telemetry: bool,

    /// Язык UI.
    #[setting(
        id = "ui.language",
        path = "ui.language",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.language.label",
        label_ru = "Язык UI",
        description_id = "settings.ui.language.description",
        description_ru = "Короткий код языка UI, например `ru` или `en`.",
        editor = "text",
        min_len = crate::validation::MIN_UI_LANGUAGE_LEN,
        max_len = crate::validation::MAX_UI_LANGUAGE_LEN,
        apply = "ui.apply"
    )]
    pub language: String,

    /// Идентификатор skin-а UI.
    #[setting(
        id = "ui.skin",
        path = "ui.skin",
        section = "ui",
        group = "shell",
        surface = "main-settings-window",
        label_id = "settings.ui.skin.label",
        label_ru = "Skin UI",
        description_id = "settings.ui.skin.description",
        description_ru = "Stable id UI skin; неизвестный id является config error.",
        editor = "select",
        apply = "ui.apply",
        options(option(
            id = "minimal",
            label_id = "settings.ui.skin.minimal",
            label_ru = "Minimal"
        ),)
    )]
    pub skin: String,

    /// Настройки кастомного заголовка окна.
    #[serde(default)]
    #[setting(nested)]
    pub window: UiWindowConfig,

    /// Настройки будущего Settings UI.
    #[serde(default)]
    #[setting(nested)]
    pub settings: UiSettingsConfig,

    /// Настройки UI-анимаций.
    #[serde(default)]
    #[setting(nested)]
    pub animations: UiAnimationsConfig,
}

impl Default for UiConfig {
    /// Возвращает русскоязычный UI по умолчанию.
    fn default() -> Self {
        Self {
            show_telemetry: true,
            language: "ru".to_string(),
            skin: "minimal".to_string(),
            window: UiWindowConfig::default(),
            settings: UiSettingsConfig::default(),
            animations: UiAnimationsConfig::default(),
        }
    }
}

/// Настройки кастомного window chrome; применяются после Apply/OK.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiWindowConfig {
    /// Высота titlebar в логических UI pixels/egui points.
    #[setting(
        id = "ui.window.titlebar_height_px",
        path = "ui.window.titlebar_height_px",
        section = "ui",
        group = "window",
        surface = "main-settings-window",
        label_id = "settings.ui.window.titlebar_height_px.label",
        label_ru = "Высота заголовка окна",
        description_id = "settings.ui.window.titlebar_height_px.description",
        description_ru = "Высота кастомного titlebar Rustiplayer в логических UI pixels.",
        help_id = "settings.ui.window.titlebar_height_px.help",
        help_ru = "Панель остаётся overlay поверх видео: viewport не сжимается и exclusion rect не добавляется.",
        editor = "integer",
        min = crate::validation::MIN_TITLEBAR_HEIGHT_PX,
        max = crate::validation::MAX_TITLEBAR_HEIGHT_PX,
        step = 1,
        unit = "px",
        apply = "ui.apply"
    )]
    pub titlebar_height_px: u16,
}

impl Default for UiWindowConfig {
    /// Возвращает высоту titlebar, близкую к обычным desktop chrome controls.
    fn default() -> Self {
        Self {
            titlebar_height_px: 40,
        }
    }
}

/// Настройки поведения Settings UI, которые применяются только после Apply/OK.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiSettingsConfig {
    /// Верхняя граница частоты live preview updates, чтобы slider не перегружал runtime.
    #[setting(
        id = "ui.settings.live_preview_max_hz",
        path = "ui.settings.live_preview_max_hz",
        section = "ui",
        group = "settings_runtime",
        surface = "main-settings-window",
        label_id = "settings.ui.settings.live_preview_max_hz.label",
        label_ru = "Частота live preview",
        description_id = "settings.ui.settings.live_preview_max_hz.description",
        description_ru = "Максимальная частота live preview updates после Apply.",
        help_id = "settings.ui.settings.live_preview_max_hz.help",
        help_ru = "Это committed setting: открытая draft/preview transaction продолжает использовать последнее применённое значение.",
        editor = "integer",
        min = crate::validation::MIN_LIVE_PREVIEW_MAX_HZ,
        max = crate::validation::MAX_LIVE_PREVIEW_MAX_HZ,
        step = 1,
        unit = "hz",
        apply = "ui.apply"
    )]
    pub live_preview_max_hz: u16,
}

impl Default for UiSettingsConfig {
    /// Возвращает плавный default без привязки к конкретному refresh rate монитора.
    fn default() -> Self {
        Self {
            live_preview_max_hz: 60,
        }
    }
}

/// Настройки UI-анимаций; применяются после Apply/OK, идущую анимацию не перезапускают.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, settings_derive::SettingsSchema,
)]
#[settings(require_all_fields)]
#[serde(default, deny_unknown_fields)]
pub struct UiAnimationsConfig {
    /// Длительность выезда/заезда settings sidebar; `0` отключает анимацию.
    #[setting(
        id = "ui.animations.sidebar_slide_duration_ms",
        path = "ui.animations.sidebar_slide_duration_ms",
        section = "ui",
        group = "animations",
        surface = "main-settings-window",
        label_id = "settings.ui.animations.sidebar_slide_duration_ms.label",
        label_ru = "Анимация сайдбара",
        description_id = "settings.ui.animations.sidebar_slide_duration_ms.description",
        description_ru = "Длительность выезда панели настроек и сжатия видео, мс.",
        help_id = "settings.ui.animations.sidebar_slide_duration_ms.help",
        help_ru = "0 отключает анимацию: панель появляется мгновенно.",
        editor = "integer",
        min = crate::validation::MIN_SIDEBAR_SLIDE_DURATION_MS,
        max = crate::validation::MAX_SIDEBAR_SLIDE_DURATION_MS,
        step = 50,
        unit = "ms",
        apply = "ui.apply"
    )]
    pub sidebar_slide_duration_ms: u16,
}

impl Default for UiAnimationsConfig {
    /// Default 0,5 сек: заметная, но не затягивающая UI анимация.
    fn default() -> Self {
        Self {
            sidebar_slide_duration_ms: 500,
        }
    }
}

#[cfg(test)]
mod settings_metadata_tests {
    use std::collections::BTreeSet;

    use settings_core::{
        DefaultBehavior, NumericRange, SelectDescriptor, SelectListDescriptor, SettingAccess,
        SettingApplyMode, SettingEditor, SettingId, SettingOptionId, SettingValue, SettingsError,
        SettingsRegistry, SettingsSchema, TextFormat,
    };

    use super::*;
    use crate::validation;

    const EXPECTED_SETTING_IDS: &[&str] = &[
        "schema_version",
        "player.start_paused",
        "player.resume_last_position",
        "player.seek.commit_timeout_ms",
        "player.seek.resume_audio_min_buffer_ms",
        "player.seek.resume_audio_gate_timeout_ms",
        "player.seek.resume_video_min_ready_frames",
        "player.seek.fast_preroll_budget_ms",
        "player.seek.fast_preroll_video_packet_burst",
        "player.seek.paused_commit_behavior",
        "player.seek.hotkey_small_step_secs",
        "player.seek.hotkey_large_step_secs",
        "player.demux.max_consecutive_corrupted_packets",
        "player.preferred_video_codec_order",
        "video.preferred_backend",
        "video.max_decode_ahead_ms",
        "video.present_queue_frames",
        "video.decoder_packet_channel_frames",
        "video.decoder_frame_channel_frames",
        "video.decoder_ready_queue_frames",
        "video.decoder_surface_pool_frames",
        "video.sw_decoder_surface_pool_frames",
        "video.sw_decode_threads",
        "video.zero_copy_surface_pool_slots",
        "video.scheduler.demux_packets_per_tick",
        "video.scheduler.video_packets_per_tick",
        "video.scheduler.decoded_frames_per_tick",
        "video.scheduler.catch_up_budget_ms",
        "video.scheduler.present_queue_min_frames",
        "video.scheduler.present_queue_target_frames",
        "video.scheduler.decode_ahead_target_ms",
        "video.scheduler.surface_free_slots_min",
        "video.scheduler.surface_free_slots_target",
        "frame_server.hover_preview_enabled",
        "frame_server.hover_pool_frames",
        "frame_server.hover_thread_count",
        "frame_server.hover_prepare_window_slots",
        "frame_server.software_hover_prepare_window_slots",
        "frame_server.recent_superseded_prepare_slots",
        "frame_server.software_recent_superseded_prepare_slots",
        "frame_server.hover_leave_grace_ms",
        "frame_server.network_hover_prepare_throttle_ms",
        "frame_server.live_scrub_enabled",
        "frame_server.live_scrub_decode_mode",
        "frame_server.live_scrub_max_hz",
        "render.profile",
        "render.hdr_to_sdr.enabled",
        "render.hdr_to_sdr.operator",
        "render.hdr_to_sdr.sdr_reference_white_nits",
        "render.hdr_to_sdr.hdr_reference_peak_nits",
        "render.tone_mapping",
        "render.color_adjustment.brightness",
        "render.color_adjustment.contrast",
        "render.color_adjustment.saturation",
        "render.color_adjustment.exposure",
        "render.color_adjustment.rgb_gain",
        "render.color_adjustment.rgb_offset",
        "render.vulkan.present_mode",
        "render.vulkan.max_frame_latency",
        "render.opengles.enabled",
        "render.opengles.simple_ui",
        "audio.volume",
        "audio.output_device",
        "audio.buffer_target_ms",
        "network.memory_cache_mb",
        "network.read_ahead_mb",
        "network.prefetch_initial_chunk_kb",
        "network.prefetch_chunk_mb",
        "network.connect_timeout_ms",
        "network.read_timeout_ms",
        "youtube.enabled",
        "youtube.prefer_account_session",
        "youtube.resolve_timeout_ms",
        "ui.show_telemetry",
        "ui.language",
        "ui.skin",
        "ui.window.titlebar_height_px",
        "ui.settings.live_preview_max_hz",
        "ui.animations.sidebar_slide_duration_ms",
    ];

    #[test]
    fn app_config_registry_covers_all_current_leaf_fields() {
        let registry = registry();
        let actual_ids = registry
            .descriptors()
            .map(|descriptor| descriptor.id.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        let expected_ids = EXPECTED_SETTING_IDS
            .iter()
            .map(|id| (*id).to_owned())
            .collect::<BTreeSet<_>>();

        assert_eq!(actual_ids, expected_ids);
    }

    #[test]
    fn schema_version_is_read_only_system_setting() {
        let registry = registry();
        let descriptor = descriptor(&registry, "schema_version");

        assert_eq!(descriptor.access, SettingAccess::ReadOnly);
        assert_eq!(descriptor.default_behavior, DefaultBehavior::NoReset);
        assert_eq!(descriptor.placement.section.as_str(), "system");
        assert_eq!(descriptor.placement.group.as_str(), "schema");
        assert!(matches!(descriptor.editor, SettingEditor::ReadOnly));

        let mut config = AppConfig::default();
        let error = registry
            .set_value(
                &mut config,
                &SettingId::from("schema_version"),
                SettingValue::Integer(3),
            )
            .expect_err("schema_version must reject writes");
        assert_eq!(
            error,
            SettingsError::ReadOnlySetting {
                id: SettingId::from("schema_version"),
            }
        );
        assert_eq!(config.schema_version, CURRENT_SCHEMA_VERSION);
    }

    #[test]
    fn user_visible_metadata_has_text_and_placement() {
        let registry = registry();

        for descriptor in registry.descriptors() {
            assert!(
                descriptor
                    .text
                    .label
                    .text_id
                    .as_str()
                    .starts_with("settings."),
                "{} label text id must be stable",
                descriptor.id,
            );
            assert!(
                !descriptor.text.label.fallback_ru.trim().is_empty(),
                "{} label fallback must be present",
                descriptor.id,
            );
            assert!(
                !descriptor.placement.section.as_str().is_empty(),
                "{} section must be present",
                descriptor.id,
            );
            assert!(
                !descriptor.placement.group.as_str().is_empty(),
                "{} group must be present",
                descriptor.id,
            );
            assert_eq!(
                descriptor.placement.preferred_surface.as_str(),
                "main-settings-window"
            );
        }
    }

    #[test]
    fn audio_volume_metadata_names_default_volume_not_current_runtime_volume() {
        let registry = registry();
        let descriptor = descriptor(&registry, "audio.volume");

        assert!(descriptor.text.label.fallback_ru.contains("по умолчанию"));
        let description = descriptor
            .text
            .description
            .as_ref()
            .expect("audio.volume description must exist");
        assert!(description.fallback_ru.contains("Стартовая громкость"));
        let help = descriptor
            .text
            .help
            .as_ref()
            .expect("audio.volume help must exist");
        assert!(
            help.fallback_ru
                .contains("не перезаписывает текущую громкость")
        );
    }

    #[test]
    fn live_preview_mode_is_limited_to_render_color_and_hdr_fields() {
        let registry = registry();
        let preview_ids = registry
            .descriptors()
            .filter(|descriptor| descriptor.apply_mode == SettingApplyMode::ImmediatePreview)
            .map(|descriptor| descriptor.id.as_str())
            .collect::<Vec<_>>();

        assert!(!preview_ids.is_empty());
        for id in preview_ids {
            assert!(
                id.starts_with("render.color_adjustment.") || id.starts_with("render.hdr_to_sdr."),
                "{id} must not be live preview"
            );
        }

        assert_eq!(
            descriptor(&registry, "ui.settings.live_preview_max_hz").apply_mode,
            SettingApplyMode::CommittedApply
        );
    }

    #[test]
    fn rgb_fields_are_fixed_length_three_vectors() {
        let registry = registry();

        assert_vector_range(
            &registry,
            "render.color_adjustment.rgb_gain",
            validation::MIN_RENDER_RGB_GAIN,
            validation::MAX_RENDER_RGB_GAIN,
            validation::RGB_CHANNEL_COUNT,
        );
        assert_vector_range(
            &registry,
            "render.color_adjustment.rgb_offset",
            validation::MIN_RENDER_RGB_OFFSET,
            validation::MAX_RENDER_RGB_OFFSET,
            validation::RGB_CHANNEL_COUNT,
        );
    }

    #[test]
    fn metadata_ranges_match_validation_constants() {
        let registry = registry();

        assert_integer_range(
            &registry,
            "player.seek.commit_timeout_ms",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "player.seek.resume_video_min_ready_frames",
            1_usize,
            validation::MAX_SEEK_RESUME_VIDEO_READY_FRAMES,
        );
        assert_integer_range(
            &registry,
            "player.seek.resume_audio_min_buffer_ms",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "player.seek.resume_audio_gate_timeout_ms",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "player.seek.fast_preroll_budget_ms",
            1_u64,
            validation::MAX_SEEK_FAST_PREROLL_BUDGET_MS,
        );
        assert_integer_range(
            &registry,
            "player.seek.fast_preroll_video_packet_burst",
            1_usize,
            validation::MAX_SEEK_FAST_PREROLL_VIDEO_PACKET_BURST,
        );
        assert_integer_range(
            &registry,
            "player.seek.hotkey_small_step_secs",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "player.seek.hotkey_large_step_secs",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "player.demux.max_consecutive_corrupted_packets",
            1_usize,
            validation::MAX_CONSECUTIVE_CORRUPTED_PACKETS,
        );
        assert_integer_range(
            &registry,
            "video.max_decode_ahead_ms",
            validation::MIN_DECODE_AHEAD_MS,
            validation::MAX_DECODE_AHEAD_MS,
        );
        assert_integer_range(
            &registry,
            "video.present_queue_frames",
            validation::MIN_PRESENT_QUEUE_FRAMES,
            validation::MAX_PRESENT_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.decoder_packet_channel_frames",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_DECODER_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.decoder_frame_channel_frames",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_DECODER_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.decoder_ready_queue_frames",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_DECODER_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.decoder_surface_pool_frames",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_DECODER_SURFACE_POOL_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.sw_decoder_surface_pool_frames",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_DECODER_SURFACE_POOL_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.sw_decode_threads",
            validation::MIN_SW_DECODE_THREADS,
            validation::MAX_SW_DECODE_THREADS,
        );
        assert_integer_range(
            &registry,
            "video.zero_copy_surface_pool_slots",
            validation::MIN_DECODER_QUEUE_FRAMES,
            validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.demux_packets_per_tick",
            1_usize,
            validation::MAX_SCHEDULER_DEMUX_PACKETS_PER_TICK,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.video_packets_per_tick",
            1_usize,
            validation::MAX_SCHEDULER_VIDEO_PACKETS_PER_TICK,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.decoded_frames_per_tick",
            1_usize,
            validation::MAX_SCHEDULER_DECODED_FRAMES_PER_TICK,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.catch_up_budget_ms",
            1_u64,
            validation::MAX_SCHEDULER_CATCH_UP_BUDGET_MS,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.present_queue_min_frames",
            1_usize,
            validation::MAX_PRESENT_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.present_queue_target_frames",
            1_usize,
            validation::MAX_PRESENT_QUEUE_FRAMES,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.decode_ahead_target_ms",
            validation::MIN_DECODE_AHEAD_MS,
            validation::MAX_DECODE_AHEAD_MS,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.surface_free_slots_min",
            0_usize,
            validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        );
        assert_integer_range(
            &registry,
            "video.scheduler.surface_free_slots_target",
            0_usize,
            validation::MAX_ZERO_COPY_SURFACE_POOL_SLOTS,
        );
        assert_integer_range(
            &registry,
            "frame_server.hover_prepare_window_slots",
            1_u8,
            validation::MAX_FRAME_SERVER_HOVER_PREPARE_WINDOW_SLOTS,
        );
        assert_integer_range(
            &registry,
            "frame_server.software_hover_prepare_window_slots",
            1_u8,
            validation::MAX_FRAME_SERVER_SOFTWARE_HOVER_PREPARE_WINDOW_SLOTS,
        );
        assert_integer_range(
            &registry,
            "frame_server.recent_superseded_prepare_slots",
            0_u8,
            validation::MAX_FRAME_SERVER_RECENT_SUPERSEDED_PREPARE_SLOTS,
        );
        assert_integer_range(
            &registry,
            "frame_server.software_recent_superseded_prepare_slots",
            0_u8,
            validation::MAX_FRAME_SERVER_SOFTWARE_RECENT_SUPERSEDED_PREPARE_SLOTS,
        );
        assert_integer_range(
            &registry,
            "frame_server.hover_leave_grace_ms",
            validation::MIN_FRAME_SERVER_HOVER_LEAVE_GRACE_MS,
            validation::MAX_FRAME_SERVER_HOVER_LEAVE_GRACE_MS,
        );
        assert_integer_range(
            &registry,
            "frame_server.network_hover_prepare_throttle_ms",
            validation::MIN_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS,
            validation::MAX_FRAME_SERVER_NETWORK_HOVER_PREPARE_THROTTLE_MS,
        );
        assert_integer_range(
            &registry,
            "frame_server.live_scrub_max_hz",
            validation::MIN_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
            validation::MAX_FRAME_SERVER_LIVE_SCRUB_MAX_HZ,
        );
        assert_float_range(
            &registry,
            "render.hdr_to_sdr.sdr_reference_white_nits",
            validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
            validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
        );
        assert_float_range(
            &registry,
            "render.hdr_to_sdr.hdr_reference_peak_nits",
            validation::MIN_HDR_TO_SDR_REFERENCE_NITS,
            validation::MAX_HDR_TO_SDR_REFERENCE_NITS,
        );
        assert_float_range(
            &registry,
            "render.color_adjustment.brightness",
            validation::MIN_RENDER_COLOR_BRIGHTNESS,
            validation::MAX_RENDER_COLOR_BRIGHTNESS,
        );
        assert_float_range(
            &registry,
            "render.color_adjustment.contrast",
            validation::MIN_RENDER_COLOR_CONTRAST,
            validation::MAX_RENDER_COLOR_CONTRAST,
        );
        assert_float_range(
            &registry,
            "render.color_adjustment.saturation",
            validation::MIN_RENDER_COLOR_SATURATION,
            validation::MAX_RENDER_COLOR_SATURATION,
        );
        assert_float_range(
            &registry,
            "render.color_adjustment.exposure",
            validation::MIN_RENDER_COLOR_EXPOSURE,
            validation::MAX_RENDER_COLOR_EXPOSURE,
        );
        assert_vector_range(
            &registry,
            "render.color_adjustment.rgb_gain",
            validation::MIN_RENDER_RGB_GAIN,
            validation::MAX_RENDER_RGB_GAIN,
            validation::RGB_CHANNEL_COUNT,
        );
        assert_vector_range(
            &registry,
            "render.color_adjustment.rgb_offset",
            validation::MIN_RENDER_RGB_OFFSET,
            validation::MAX_RENDER_RGB_OFFSET,
            validation::RGB_CHANNEL_COUNT,
        );
        assert_integer_range(
            &registry,
            "render.vulkan.max_frame_latency",
            1_u32,
            validation::MAX_VULKAN_FRAME_LATENCY,
        );
        assert_float_range(
            &registry,
            "audio.volume",
            validation::MIN_AUDIO_VOLUME,
            validation::MAX_AUDIO_VOLUME,
        );
        assert_integer_range(
            &registry,
            "audio.buffer_target_ms",
            validation::MIN_AUDIO_BUFFER_TARGET_MS,
            validation::MAX_AUDIO_BUFFER_TARGET_MS,
        );
        assert_integer_range(
            &registry,
            "network.memory_cache_mb",
            0_u64,
            validation::MAX_NETWORK_MEMORY_CACHE_MB,
        );
        assert_integer_range(
            &registry,
            "network.read_ahead_mb",
            1_u64,
            validation::MAX_NETWORK_READ_AHEAD_MB,
        );
        assert_integer_range(
            &registry,
            "network.prefetch_initial_chunk_kb",
            1_u64,
            validation::MAX_NETWORK_PREFETCH_INITIAL_CHUNK_KB,
        );
        assert_integer_range(
            &registry,
            "network.prefetch_chunk_mb",
            1_u64,
            validation::MAX_NETWORK_READ_AHEAD_MB,
        );
        assert_integer_range(
            &registry,
            "network.connect_timeout_ms",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "network.read_timeout_ms",
            validation::MIN_POSITIVE_U64_SETTING_VALUE,
            validation::MAX_POSITIVE_U64_SETTING_VALUE,
        );
        assert_integer_range(
            &registry,
            "youtube.resolve_timeout_ms",
            1_u64,
            validation::MAX_YOUTUBE_RESOLVE_TIMEOUT_MS,
        );
        assert_text_len(
            &registry,
            "ui.language",
            validation::MIN_UI_LANGUAGE_LEN,
            validation::MAX_UI_LANGUAGE_LEN,
        );
        assert_integer_range(
            &registry,
            "ui.settings.live_preview_max_hz",
            validation::MIN_LIVE_PREVIEW_MAX_HZ,
            validation::MAX_LIVE_PREVIEW_MAX_HZ,
        );
        assert_integer_range(
            &registry,
            "ui.animations.sidebar_slide_duration_ms",
            validation::MIN_SIDEBAR_SLIDE_DURATION_MS,
            validation::MAX_SIDEBAR_SLIDE_DURATION_MS,
        );
        assert_integer_range(
            &registry,
            "ui.window.titlebar_height_px",
            validation::MIN_TITLEBAR_HEIGHT_PX,
            validation::MAX_TITLEBAR_HEIGHT_PX,
        );
    }

    #[test]
    fn static_enum_and_string_options_use_stable_ids() {
        let registry = registry();

        assert_select_options(
            &registry,
            "player.seek.paused_commit_behavior",
            &["stay_paused"],
        );
        assert_select_list_options(
            &registry,
            "player.preferred_video_codec_order",
            &["vp9", "av1", "h264", "h265", "vp8"],
        );
        assert_select_options(
            &registry,
            "video.preferred_backend",
            &["auto", "hardware", "software"],
        );
        assert_select_options(&registry, "render.profile", &["auto", "vulkan", "opengles"]);
        assert_select_options(&registry, "render.hdr_to_sdr.operator", &["bt2446_c"]);
        assert_select_options(&registry, "render.tone_mapping", &["auto", "disabled"]);
        assert_select_options(
            &registry,
            "render.vulkan.present_mode",
            &["auto", "fifo", "mailbox", "immediate"],
        );
        assert_select_options(
            &registry,
            "frame_server.live_scrub_decode_mode",
            &["throttled_latest", "every_drag_event"],
        );
        assert_select_options(&registry, "ui.skin", &[validation::DEFAULT_UI_SKIN]);
    }

    #[test]
    fn frame_server_metadata_is_editable_and_metadata_ready() {
        let registry = registry();
        let frame_server_ids = EXPECTED_SETTING_IDS
            .iter()
            .copied()
            .filter(|id| id.starts_with("frame_server."))
            .collect::<Vec<_>>();

        assert_eq!(frame_server_ids.len(), 12);
        for id in frame_server_ids {
            let descriptor = descriptor(&registry, id);
            assert_eq!(descriptor.access, SettingAccess::ReadWrite);
            assert_eq!(
                descriptor.default_behavior,
                DefaultBehavior::FromDefaultDocument
            );
            assert_eq!(descriptor.placement.section.as_str(), "frame_server");
            assert!(!descriptor.placement.group_default_open);
            assert_eq!(descriptor.route.as_str(), "frame_server.apply");
            assert_eq!(descriptor.apply_mode, SettingApplyMode::CommittedApply);
            assert_russian_fallback(&descriptor.text.label.fallback_ru, id);
            let description =
                descriptor.text.description.as_ref().unwrap_or_else(|| {
                    panic!("{id} description must explain frame_server behavior")
                });
            assert_russian_fallback(&description.fallback_ru, id);
            let help = descriptor
                .text
                .help
                .as_ref()
                .unwrap_or_else(|| panic!("{id} help must explain S30B limitations"));
            assert_russian_fallback(&help.fallback_ru, id);
        }

        for id in [
            "frame_server.hover_pool_frames",
            "frame_server.hover_thread_count",
        ] {
            let SettingEditor::AutoFixedPositiveInteger(editor) = &descriptor(&registry, id).editor
            else {
                panic!("{id} must use Auto/Fixed-positive budget UI");
            };
            assert_eq!(editor.auto_label.fallback_ru, "Авто");
            assert_eq!(editor.fixed_label.fallback_ru, "Фиксированно");
        }

        assert_eq!(
            registry
                .get_value(
                    &AppConfig::default(),
                    &SettingId::from("frame_server.hover_pool_frames")
                )
                .expect("budget metadata value readable"),
            SettingValue::Text("auto".to_owned())
        );

        let mut config = AppConfig::default();
        registry
            .set_value(
                &mut config,
                &SettingId::from("frame_server.live_scrub_enabled"),
                SettingValue::Bool(false),
            )
            .expect("frame_server bool setting must be writable in S30B");
        registry
            .set_value(
                &mut config,
                &SettingId::from("frame_server.hover_pool_frames"),
                SettingValue::Text("12".to_owned()),
            )
            .expect("frame_server fixed budget setting must be writable in S30B");
        assert!(!config.frame_server.live_scrub_enabled);
        assert_eq!(
            config.frame_server.hover_pool_frames,
            crate::frame_server::FrameServerBudgetConfig::Fixed(12)
        );
    }

    #[test]
    fn video_backend_options_do_not_expose_implementation_specific_public_id() {
        let registry = registry();
        let SettingEditor::Select(SelectDescriptor::Static { options }) =
            &descriptor(&registry, "video.preferred_backend").editor
        else {
            panic!("video.preferred_backend must use static select editor");
        };

        let ids = option_ids(options);
        let implementation_specific_underscore_id = ["ffmpeg", "sw"].join("_");
        let implementation_specific_dash_id = ["ffmpeg", "sw"].join("-");

        assert_eq!(ids, vec!["auto", "hardware", "software"]);
        assert!(!ids.contains(&implementation_specific_underscore_id.as_str()));
        assert!(!ids.contains(&implementation_specific_dash_id.as_str()));
    }

    #[test]
    fn generated_accessors_read_and_reset_values_from_default_documents() {
        let registry = registry();
        let default_config = AppConfig::default();

        assert_eq!(
            registry
                .get_value(&default_config, &SettingId::from("audio.volume"))
                .expect("default audio.volume should be readable"),
            SettingValue::Float(default_config.audio.volume)
        );
        assert_eq!(
            registry
                .get_value(
                    &default_config,
                    &SettingId::from("player.preferred_video_codec_order"),
                )
                .expect("default codec order should be readable"),
            SettingValue::SelectList(vec![
                SettingOptionId::from("vp9"),
                SettingOptionId::from("av1"),
                SettingOptionId::from("h264"),
                SettingOptionId::from("h265"),
                SettingOptionId::from("vp8"),
            ])
        );

        let mut config = AppConfig::default();
        config.audio.volume = 0.25;
        let mut custom_default = AppConfig::default();
        custom_default.audio.volume = 0.5;

        registry
            .reset_value(
                &mut config,
                &custom_default,
                &SettingId::from("audio.volume"),
            )
            .expect("reset should read from provided default document");
        assert_eq!(config.audio.volume, custom_default.audio.volume);
    }

    fn registry() -> SettingsRegistry<AppConfig> {
        AppConfig::settings_registry().expect("AppConfig registry should be generated")
    }

    fn descriptor<'registry>(
        registry: &'registry SettingsRegistry<AppConfig>,
        id: &str,
    ) -> &'registry settings_core::SettingDescriptor {
        registry
            .descriptor(&SettingId::from(id))
            .unwrap_or_else(|| panic!("{id} descriptor should exist"))
    }

    fn assert_integer_range<Min, Max>(
        registry: &SettingsRegistry<AppConfig>,
        id: &str,
        expected_min: Min,
        expected_max: Max,
    ) where
        Min: TryInto<i64>,
        Max: TryInto<i64>,
        Min::Error: std::fmt::Debug,
        Max::Error: std::fmt::Debug,
    {
        let SettingEditor::Numeric(numeric) = &descriptor(registry, id).editor else {
            panic!("{id} must use numeric editor");
        };
        let NumericRange::Integer { min, max } = &numeric.range else {
            panic!("{id} must use integer range");
        };
        assert_eq!(*min, expected_min.try_into().expect("min must fit i64"));
        assert_eq!(*max, expected_max.try_into().expect("max must fit i64"));
    }

    fn assert_float_range<Min, Max>(
        registry: &SettingsRegistry<AppConfig>,
        id: &str,
        expected_min: Min,
        expected_max: Max,
    ) where
        Min: Into<f64>,
        Max: Into<f64>,
    {
        let SettingEditor::Numeric(numeric) = &descriptor(registry, id).editor else {
            panic!("{id} must use numeric editor");
        };
        let NumericRange::Float { min, max } = &numeric.range else {
            panic!("{id} must use float range");
        };
        assert_eq!(*min, expected_min.into());
        assert_eq!(*max, expected_max.into());
    }

    fn assert_vector_range<Min, Max>(
        registry: &SettingsRegistry<AppConfig>,
        id: &str,
        expected_min: Min,
        expected_max: Max,
        expected_len: usize,
    ) where
        Min: Into<f64>,
        Max: Into<f64>,
    {
        let SettingEditor::Vector(vector) = &descriptor(registry, id).editor else {
            panic!("{id} must use vector editor");
        };
        let NumericRange::Float { min, max } = &vector.element.range else {
            panic!("{id} vector elements must use float range");
        };
        assert_eq!(*min, expected_min.into());
        assert_eq!(*max, expected_max.into());
        assert_eq!(vector.expected_len, expected_len);
    }

    fn assert_text_len(
        registry: &SettingsRegistry<AppConfig>,
        id: &str,
        expected_min_len: usize,
        expected_max_len: usize,
    ) {
        let SettingEditor::Text(text) = &descriptor(registry, id).editor else {
            panic!("{id} must use text editor");
        };
        assert_eq!(text.format, TextFormat::SingleLine);
        assert_eq!(text.min_len, Some(expected_min_len));
        assert_eq!(text.max_len, Some(expected_max_len));
    }

    fn assert_select_options(registry: &SettingsRegistry<AppConfig>, id: &str, expected: &[&str]) {
        let SettingEditor::Select(SelectDescriptor::Static { options }) =
            &descriptor(registry, id).editor
        else {
            panic!("{id} must use static select editor");
        };
        assert_eq!(option_ids(options), expected);
    }

    fn assert_select_list_options(
        registry: &SettingsRegistry<AppConfig>,
        id: &str,
        expected: &[&str],
    ) {
        let SettingEditor::SelectList(SelectListDescriptor { options, .. }) =
            &descriptor(registry, id).editor
        else {
            panic!("{id} must use static select-list editor");
        };
        assert_eq!(option_ids(options), expected);
    }

    fn assert_russian_fallback(text: &str, id: &str) {
        assert!(
            text.chars().any(|character| {
                ('а'..='я').contains(&character)
                    || ('А'..='Я').contains(&character)
                    || character == 'ё'
                    || character == 'Ё'
            }),
            "{id} fallback must contain Russian text",
        );
    }

    fn option_ids(options: &[settings_core::SettingOption]) -> Vec<&str> {
        options.iter().map(|option| option.id.as_str()).collect()
    }
}
