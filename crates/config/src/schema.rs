use serde::{Deserialize, Deserializer, Serialize};

use crate::{ConfigResult, validation};

/// Текущая версия TOML-схемы.
pub const CURRENT_SCHEMA_VERSION: u32 = 2;

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
        document_schema_version_2_defaults(&mut toml_text);

        Ok(toml_text)
    }
}

/// Добавляет русские комментарии к новым полям schema version 2 в default TOML.
fn document_schema_version_2_defaults(toml_text: &mut String) {
    insert_default_config_comment(
        toml_text,
        "[player.seek]",
        "# Настройки live seek и interactive scrub.",
    );
    insert_default_config_comment(
        toml_text,
        "live_interval_ms = 33",
        "# Минимальный интервал между live scrub preview-командами.",
    );
    insert_default_config_comment(
        toml_text,
        "live_preview_budget_ms = 100",
        "# Budget preview work на один live scrub update.",
    );
    insert_default_config_comment(
        toml_text,
        "timeline_release_policy = \"visible-preview\"",
        "# Политика отпускания timeline: visible-preview быстрее, latest-target точнее.",
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
        "read_ahead_mb = 64",
        "# Network read-ahead budget.",
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

    /// Зарезервировано для будущего восстановления позиции.
    pub resume_last_position: bool,

    /// Настройки seek/scrub поведения.
    pub seek: PlayerSeekConfig,

    /// Настройки demuxer fail-safe поведения.
    pub demux: PlayerDemuxConfig,

    /// Приоритет codec candidates при выборе video stream.
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerDemuxConfig {
    /// Сколько corrupted packets подряд demuxer может пропустить до fatal ошибки.
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

/// Настройки live seek и interactive scrub.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PlayerSeekConfig {
    /// Интервал live scrub update-команд.
    pub live_interval_ms: u64,

    /// Бюджет preview work на один live scrub update.
    pub live_preview_budget_ms: u64,

    /// Политика commit-а при отпускании pointer-а на timeline.
    pub timeline_release_policy: TimelineReleasePolicy,

    /// Timeout финального commit-а seek/scrub.
    pub commit_timeout_ms: u64,

    /// Минимальный audio buffer перед resume после commit-а.
    pub resume_audio_min_buffer_ms: u64,

    /// Soft timeout audio gate-а после показанного target video frame.
    pub resume_audio_gate_timeout_ms: u64,

    /// Минимальный запас готовых video frames перед resume после commit-а.
    pub resume_video_min_ready_frames: usize,

    /// Поведение commit-а, если playback был на паузе.
    pub paused_commit_behavior: PausedCommitBehavior,

    /// Малый шаг горячих клавиш seek.
    pub hotkey_small_step_secs: u64,

    /// Большой шаг горячих клавиш seek.
    pub hotkey_large_step_secs: u64,
}

impl Default for PlayerSeekConfig {
    /// Возвращает documented defaults live seek плана.
    fn default() -> Self {
        Self {
            live_interval_ms: 33,
            live_preview_budget_ms: 100,
            timeline_release_policy: TimelineReleasePolicy::VisiblePreview,
            commit_timeout_ms: 10_000,
            resume_audio_min_buffer_ms: 50,
            resume_audio_gate_timeout_ms: 250,
            resume_video_min_ready_frames: 3,
            paused_commit_behavior: PausedCommitBehavior::StayPaused,
            hotkey_small_step_secs: 5,
            hotkey_large_step_secs: 30,
        }
    }
}

/// Политика отпускания timeline pointer-а, сохранённая в TOML config.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TimelineReleasePolicy {
    /// Быстрый UX: продолжить playback с последнего реально показанного preview frame.
    VisiblePreview,

    /// Точный UX: всегда завершить seek в последнюю target-позицию scrub-а.
    LatestTarget,
}

impl Default for TimelineReleasePolicy {
    /// По умолчанию timeline release остаётся latency-first.
    fn default() -> Self {
        Self::VisiblePreview
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

    /// Bounded packet channel между worker и decoder thread.
    pub decoder_packet_channel_frames: usize,

    /// Bounded decoded frame channel между decoder thread и worker.
    pub decoder_frame_channel_frames: usize,

    /// Backend-local ready queue для burst `FrameReady` events.
    pub decoder_ready_queue_frames: usize,

    /// Количество VA output surface descriptors для hardware decoder-а.
    pub decoder_surface_pool_frames: usize,

    /// Количество zero-copy external import slots.
    pub zero_copy_surface_pool_slots: usize,

    /// Настройки worker scheduler-а и bounded catch-up policy.
    pub scheduler: VideoSchedulerConfig,
}

impl Default for VideoConfig {
    /// Возвращает текущие MVP-лимиты video backpressure.
    fn default() -> Self {
        Self {
            hardware_decode_only: true,
            preferred_backend: VideoBackendPreference::Auto,
            max_decode_ahead_ms: 500,
            present_queue_frames: 8,
            decoder_packet_channel_frames: 32,
            decoder_frame_channel_frames: 8,
            decoder_ready_queue_frames: 8,
            decoder_surface_pool_frames: 24,
            zero_copy_surface_pool_slots: 24,
            scheduler: VideoSchedulerConfig::default(),
        }
    }
}

/// Scheduler-настройки video pipeline без codec/backend-specific имён.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct VideoSchedulerConfig {
    /// Базовый budget чтения container packets за один worker tick.
    pub demux_packets_per_tick: usize,

    /// Базовый budget отправки video packets в decoder thread за tick.
    pub video_packets_per_tick: usize,

    /// Базовый budget приёма decoded frames из decoder thread за tick.
    pub decoded_frames_per_tick: usize,

    /// Дополнительное bounded окно catch-up work после обычного tick.
    pub catch_up_budget_ms: u64,

    /// Минимальный запас ready frames, ниже которого pipeline считается starvation-prone.
    pub present_queue_min_frames: usize,

    /// Целевой запас ready frames; max задаётся `VideoConfig::present_queue_frames`.
    pub present_queue_target_frames: usize,

    /// Целевой decode-ahead; max задаётся `VideoConfig::max_decode_ahead_ms`.
    pub decode_ahead_target_ms: u64,

    /// Минимальный резерв свободных zero-copy surface/import slots перед decode.
    pub surface_free_slots_min: usize,

    /// Целевой резерв свободных zero-copy surface/import slots для catch-up.
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
    /// Размер RAM cache; `0` явно отключает RAM cache.
    pub memory_cache_mb: u64,

    /// Максимальный read-ahead для сетевых источников.
    pub read_ahead_mb: u64,

    /// Timeout подключения к сетевому источнику.
    pub connect_timeout_ms: u64,

    /// Timeout чтения из сетевого источника.
    pub read_timeout_ms: u64,
}

impl Default for NetworkConfig {
    /// Возвращает conservative cache defaults.
    fn default() -> Self {
        Self {
            memory_cache_mb: 128,
            read_ahead_mb: 64,
            connect_timeout_ms: 15_000,
            read_timeout_ms: 15_000,
        }
    }
}

/// Настройки YouTube/service слоя.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct YoutubeConfig {
    /// Разрешает YouTube adapter.
    pub enabled: bool,

    /// Предпочитать account/session cookies, если service adapter их поддерживает.
    pub prefer_account_session: bool,

    /// Максимальное время подготовки direct stream metadata через `yt-dlp`.
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Показывать диагностическую панель telemetry.
    pub show_telemetry: bool,

    /// Язык UI.
    pub language: String,

    /// Идентификатор skin-а UI.
    pub skin: String,
}

impl Default for UiConfig {
    /// Возвращает русскоязычный UI по умолчанию.
    fn default() -> Self {
        Self {
            show_telemetry: true,
            language: "ru".to_string(),
            skin: "minimal".to_string(),
        }
    }
}
