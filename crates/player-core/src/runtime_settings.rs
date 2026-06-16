use std::fmt;
use std::time::Duration;

use crate::{PlayerTickConfig, PlayerVideoDecoderThreadConfig};

/// Typed id настройки player runtime, который можно вернуть в apply report.
///
/// Эти id совпадают с user-facing config paths там, где настройка уже есть в
/// TOML. Для owner-level групп, которые пока не являются отдельным TOML-полем,
/// используются внутренние stable ids с префиксом `player.runtime`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayerRuntimeSettingId {
    /// Весь worker-owned `PlayerTickConfig` как internal atomic group.
    PlayerTickConfig,

    /// Default volume для startup/future-media policy, не текущая runtime громкость.
    AudioDefaultVolume,

    /// Весь decoder-thread config как internal controlled-rebuild group.
    PlayerDecoderThreadConfig,

    /// `player.preferred_video_codec_order`.
    PlayerPreferredVideoCodecOrder,

    /// `player.demux.max_consecutive_corrupted_packets`.
    PlayerDemuxMaxConsecutiveCorruptedPackets,

    /// `player.seek.commit_timeout_ms`.
    PlayerSeekCommitTimeoutMs,

    /// `player.seek.resume_audio_min_buffer_ms`.
    PlayerSeekResumeAudioMinBufferMs,

    /// `player.seek.resume_audio_gate_timeout_ms`.
    PlayerSeekResumeAudioGateTimeoutMs,

    /// `player.seek.resume_video_min_ready_frames`.
    PlayerSeekResumeVideoMinReadyFrames,

    /// `player.seek.fast_preroll_budget_ms`.
    PlayerSeekFastPrerollBudgetMs,

    /// `player.seek.fast_preroll_video_packet_burst`.
    PlayerSeekFastPrerollVideoPacketBurst,

    /// `audio.buffer_target_ms`.
    AudioBufferTargetMs,

    /// `video.max_decode_ahead_ms`.
    VideoMaxDecodeAheadMs,

    /// `video.present_queue_frames`.
    VideoPresentQueueFrames,

    /// `video.decoder_packet_channel_frames`.
    VideoDecoderPacketChannelFrames,

    /// `video.decoder_frame_channel_frames`.
    VideoDecoderFrameChannelFrames,

    /// `video.decoder_ready_queue_frames`.
    VideoDecoderReadyQueueFrames,

    /// `video.decoder_surface_pool_frames`.
    VideoDecoderSurfacePoolFrames,

    /// `video.zero_copy_surface_pool_slots`.
    VideoZeroCopySurfacePoolSlots,

    /// `video.scheduler.demux_packets_per_tick`.
    VideoSchedulerDemuxPacketsPerTick,

    /// `video.scheduler.video_packets_per_tick`.
    VideoSchedulerVideoPacketsPerTick,

    /// `video.scheduler.decoded_frames_per_tick`.
    VideoSchedulerDecodedFramesPerTick,

    /// `video.scheduler.catch_up_budget_ms`.
    VideoSchedulerCatchUpBudgetMs,

    /// `video.scheduler.present_queue_min_frames`.
    VideoSchedulerPresentQueueMinFrames,

    /// `video.scheduler.present_queue_target_frames`.
    VideoSchedulerPresentQueueTargetFrames,

    /// `video.scheduler.decode_ahead_target_ms`.
    VideoSchedulerDecodeAheadTargetMs,

    /// `video.scheduler.surface_free_slots_min`.
    VideoSchedulerSurfaceFreeSlotsMin,

    /// `video.scheduler.surface_free_slots_target`.
    VideoSchedulerSurfaceFreeSlotsTarget,
}

impl PlayerRuntimeSettingId {
    /// Возвращает stable id/path для UI report-а и logs.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PlayerTickConfig => "player.runtime.tick_config",
            Self::AudioDefaultVolume => "audio.volume",
            Self::PlayerDecoderThreadConfig => "player.runtime.decoder_thread_config",
            Self::PlayerPreferredVideoCodecOrder => "player.preferred_video_codec_order",
            Self::PlayerDemuxMaxConsecutiveCorruptedPackets => {
                "player.demux.max_consecutive_corrupted_packets"
            }
            Self::PlayerSeekCommitTimeoutMs => "player.seek.commit_timeout_ms",
            Self::PlayerSeekResumeAudioMinBufferMs => "player.seek.resume_audio_min_buffer_ms",
            Self::PlayerSeekResumeAudioGateTimeoutMs => "player.seek.resume_audio_gate_timeout_ms",
            Self::PlayerSeekResumeVideoMinReadyFrames => {
                "player.seek.resume_video_min_ready_frames"
            }
            Self::PlayerSeekFastPrerollBudgetMs => "player.seek.fast_preroll_budget_ms",
            Self::PlayerSeekFastPrerollVideoPacketBurst => {
                "player.seek.fast_preroll_video_packet_burst"
            }
            Self::AudioBufferTargetMs => "audio.buffer_target_ms",
            Self::VideoMaxDecodeAheadMs => "video.max_decode_ahead_ms",
            Self::VideoPresentQueueFrames => "video.present_queue_frames",
            Self::VideoDecoderPacketChannelFrames => "video.decoder_packet_channel_frames",
            Self::VideoDecoderFrameChannelFrames => "video.decoder_frame_channel_frames",
            Self::VideoDecoderReadyQueueFrames => "video.decoder_ready_queue_frames",
            Self::VideoDecoderSurfacePoolFrames => "video.decoder_surface_pool_frames",
            Self::VideoZeroCopySurfacePoolSlots => "video.zero_copy_surface_pool_slots",
            Self::VideoSchedulerDemuxPacketsPerTick => "video.scheduler.demux_packets_per_tick",
            Self::VideoSchedulerVideoPacketsPerTick => "video.scheduler.video_packets_per_tick",
            Self::VideoSchedulerDecodedFramesPerTick => "video.scheduler.decoded_frames_per_tick",
            Self::VideoSchedulerCatchUpBudgetMs => "video.scheduler.catch_up_budget_ms",
            Self::VideoSchedulerPresentQueueMinFrames => "video.scheduler.present_queue_min_frames",
            Self::VideoSchedulerPresentQueueTargetFrames => {
                "video.scheduler.present_queue_target_frames"
            }
            Self::VideoSchedulerDecodeAheadTargetMs => "video.scheduler.decode_ahead_target_ms",
            Self::VideoSchedulerSurfaceFreeSlotsMin => "video.scheduler.surface_free_slots_min",
            Self::VideoSchedulerSurfaceFreeSlotsTarget => {
                "video.scheduler.surface_free_slots_target"
            }
        }
    }
}

/// Worker-owned runtime update для playback tick/scheduler limits.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerRuntimeTickConfigUpdate {
    /// Новый typed tick config, уже собранный из validated settings выше player-core.
    pub tick_config: PlayerTickConfig,

    /// Конкретные settings ids, которые привели к этому atomic update.
    pub affected_settings: Vec<PlayerRuntimeSettingId>,
}

impl PlayerRuntimeTickConfigUpdate {
    /// Создаёт tick update с явным списком затронутых settings.
    #[must_use]
    pub fn new<I>(tick_config: PlayerTickConfig, affected_settings: I) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            tick_config,
            affected_settings: collect_or_default(
                affected_settings,
                &[PlayerRuntimeSettingId::PlayerTickConfig],
            ),
        }
    }
}

/// Update default-volume policy без перезаписи текущей runtime громкости.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerRuntimeDefaultVolumeUpdate {
    /// Default volume в диапазоне `0.0..=1.0`.
    pub default_volume: f32,

    /// Конкретные settings ids, которые привели к update.
    pub affected_settings: Vec<PlayerRuntimeSettingId>,
}

impl PlayerRuntimeDefaultVolumeUpdate {
    /// Создаёт default-volume update с явным списком затронутых settings.
    #[must_use]
    pub fn new<I>(default_volume: f32, affected_settings: I) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            default_volume,
            affected_settings: collect_or_default(
                affected_settings,
                &[PlayerRuntimeSettingId::AudioDefaultVolume],
            ),
        }
    }
}

/// Decoder/channel/pool settings, которые применяются через controlled rebuild.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRuntimeDecoderThreadConfigUpdate {
    /// Новый decoder-thread config для следующего backend lifecycle.
    pub decoder_thread_config: PlayerVideoDecoderThreadConfig,

    /// Конкретные settings ids, которые привели к controlled rebuild request.
    pub affected_settings: Vec<PlayerRuntimeSettingId>,
}

impl PlayerRuntimeDecoderThreadConfigUpdate {
    /// Создаёт decoder-thread update с явным списком затронутых settings.
    #[must_use]
    pub fn new<I>(
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
        affected_settings: I,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            decoder_thread_config,
            affected_settings: collect_or_default(
                affected_settings,
                &[PlayerRuntimeSettingId::PlayerDecoderThreadConfig],
            ),
        }
    }
}

/// Typed committed update, который settings layer отправляет в player owner.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerRuntimeSettingsUpdate {
    /// In-place update для worker-owned tick config.
    pub tick_config: Option<PlayerRuntimeTickConfigUpdate>,

    /// Worker-owned default volume policy; не меняет current session volume.
    pub default_volume: Option<PlayerRuntimeDefaultVolumeUpdate>,

    /// Decoder/channel/pool settings, которые применяются через controlled rebuild.
    pub decoder_thread_config: Option<PlayerRuntimeDecoderThreadConfigUpdate>,

    /// Settings, которые player-core пока не умеет применять.
    pub unsupported_settings: Vec<PlayerRuntimeSettingId>,
}

impl PlayerRuntimeSettingsUpdate {
    /// Создаёт пустой update; apply вернёт typed `Invalid`, а не молчаливый успех.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Добавляет worker-owned tick config update.
    #[must_use]
    pub fn with_tick_config<I>(
        mut self,
        tick_config: PlayerTickConfig,
        affected_settings: I,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        self.tick_config = Some(PlayerRuntimeTickConfigUpdate::new(
            tick_config,
            affected_settings,
        ));
        self
    }

    /// Добавляет default-volume policy update.
    #[must_use]
    pub fn with_default_volume<I>(mut self, default_volume: f32, affected_settings: I) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        self.default_volume = Some(PlayerRuntimeDefaultVolumeUpdate::new(
            default_volume,
            affected_settings,
        ));
        self
    }

    /// Добавляет decoder-thread settings, которые требуют controlled rebuild.
    #[must_use]
    pub fn with_decoder_thread_config<I>(
        mut self,
        decoder_thread_config: PlayerVideoDecoderThreadConfig,
        affected_settings: I,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        self.decoder_thread_config = Some(PlayerRuntimeDecoderThreadConfigUpdate::new(
            decoder_thread_config,
            affected_settings,
        ));
        self
    }

    /// Добавляет settings, для которых в S07 нет runtime owner boundary.
    #[must_use]
    pub fn with_unsupported_settings<I>(mut self, affected_settings: I) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        self.unsupported_settings.extend(affected_settings);
        self
    }

    /// Проверяет, содержит ли update хотя бы одну meaningful group.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tick_config.is_none()
            && self.default_volume.is_none()
            && self.decoder_thread_config.is_none()
            && self.unsupported_settings.is_empty()
    }
}

/// Owner-defined atomic group внутри player runtime apply.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlayerRuntimeApplyGroup {
    /// Валидация самого request-а до групп.
    Request,

    /// Worker-owned playback tick/scheduler limits.
    TickConfig,

    /// Default-volume policy для startup/future media.
    DefaultVolume,

    /// Decoder thread/channel/pool limits.
    DecoderThreadConfig,

    /// Неподдержанные в S07 player settings.
    UnsupportedSettings,
}

/// Результат accepted group: реально изменили runtime или это no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerRuntimeAcceptedChange {
    /// Runtime state изменён.
    Applied,

    /// Update валиден, но совпадает с текущим runtime state.
    Unchanged,
}

/// Typed category результата одной atomic group.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerRuntimeApplyOutcome {
    /// Group принята и обработана в рамках текущего worker state.
    Accepted(PlayerRuntimeAcceptedChange),

    /// Нужен owner-level controlled rebuild/reconfigure; прямой мутации не было.
    RequiresControlledRebuild,

    /// Для setting пока нет supported runtime boundary.
    Unsupported,

    /// Запрошенный runtime resource отсутствует.
    AbsentResource,

    /// Update некорректен и не был применён.
    Invalid,

    /// Runtime owner встретил fatal condition.
    Fatal,
}

impl PlayerRuntimeApplyOutcome {
    /// Возвращает true только для полностью применённых/no-op групп.
    #[must_use]
    pub const fn is_accepted(self) -> bool {
        matches!(self, Self::Accepted(_))
    }
}

/// Подробный результат одной player runtime atomic group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerRuntimeApplyGroupReport {
    /// Какая owner-defined group обрабатывалась.
    pub group: PlayerRuntimeApplyGroup,

    /// Какие concrete settings привели к этому результату.
    pub affected_settings: Vec<PlayerRuntimeSettingId>,

    /// Typed category результата.
    pub outcome: PlayerRuntimeApplyOutcome,

    /// Человекочитаемое пояснение для settings status UI/logs.
    pub message: String,
}

impl PlayerRuntimeApplyGroupReport {
    /// Accepted result для валидной группы.
    #[must_use]
    pub fn accepted<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        change: PlayerRuntimeAcceptedChange,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::Accepted(change),
            message: message.into(),
        }
    }

    /// Controlled-rebuild result без прямой мутации runtime internals.
    #[must_use]
    pub fn requires_controlled_rebuild<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::RequiresControlledRebuild,
            message: message.into(),
        }
    }

    /// Unsupported result для settings без runtime boundary.
    #[must_use]
    pub fn unsupported<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::Unsupported,
            message: message.into(),
        }
    }

    /// Invalid result для rejected update.
    #[must_use]
    pub fn invalid<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::Invalid,
            message: message.into(),
        }
    }

    /// Absent-resource result для будущих apply групп с optional runtime resource.
    #[must_use]
    pub fn absent_resource<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::AbsentResource,
            message: message.into(),
        }
    }

    /// Fatal result для owner-side runtime failure.
    #[must_use]
    pub fn fatal<I>(
        group: PlayerRuntimeApplyGroup,
        affected_settings: I,
        message: impl Into<String>,
    ) -> Self
    where
        I: IntoIterator<Item = PlayerRuntimeSettingId>,
    {
        Self {
            group,
            affected_settings: affected_settings.into_iter().collect(),
            outcome: PlayerRuntimeApplyOutcome::Fatal,
            message: message.into(),
        }
    }
}

/// Полный report по player runtime apply request.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PlayerRuntimeApplyReport {
    /// Результаты owner-defined groups в порядке обработки.
    pub groups: Vec<PlayerRuntimeApplyGroupReport>,
}

impl PlayerRuntimeApplyReport {
    /// Создаёт пустой report, который caller заполнит group результатами.
    #[must_use]
    pub fn empty() -> Self {
        Self::default()
    }

    /// Добавляет результат одной atomic group.
    pub fn push(&mut self, group_report: PlayerRuntimeApplyGroupReport) {
        self.groups.push(group_report);
    }

    /// Возвращает true, если все groups были accepted.
    #[must_use]
    pub fn all_groups_accepted(&self) -> bool {
        !self.groups.is_empty()
            && self
                .groups
                .iter()
                .all(|group_report| group_report.outcome.is_accepted())
    }
}

/// Result settings-specific worker API.
pub type PlayerRuntimeApplyResult = Result<PlayerRuntimeApplyReport, PlayerRuntimeApplyError>;

/// Ошибка request/reply boundary до получения полноценного apply report-а.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlayerRuntimeApplyError {
    /// Worker command queue заполнена; update не был принят.
    Backpressure,

    /// Worker уже завершён или response channel был закрыт.
    Disconnected,

    /// Непредвиденная ошибка boundary, которую нельзя описать group report-ом.
    Fatal(String),
}

impl fmt::Display for PlayerRuntimeApplyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Backpressure => write!(formatter, "player runtime apply queue is full"),
            Self::Disconnected => write!(formatter, "player runtime apply worker is disconnected"),
            Self::Fatal(message) => write!(formatter, "player runtime apply failed: {message}"),
        }
    }
}

impl std::error::Error for PlayerRuntimeApplyError {}

/// Проверяет default volume до записи в worker-owned policy.
pub(crate) fn validate_runtime_default_volume(default_volume: f32) -> Result<(), String> {
    if !default_volume.is_finite() || !(0.0..=1.0).contains(&default_volume) {
        return Err(format!(
            "default volume must be finite and within 0.0..=1.0, got {default_volume}"
        ));
    }

    Ok(())
}

/// Проверяет runtime tick config, не полагаясь на то, что caller уже прогнал TOML validation.
pub(crate) fn validate_runtime_tick_config(tick_config: &PlayerTickConfig) -> Result<(), String> {
    ensure_non_zero(
        tick_config.max_demux_packets_per_tick,
        PlayerRuntimeSettingId::VideoSchedulerDemuxPacketsPerTick,
    )?;
    ensure_non_zero(
        tick_config.max_video_present_queue,
        PlayerRuntimeSettingId::VideoPresentQueueFrames,
    )?;
    ensure_watermark_order(
        tick_config.min_video_present_queue,
        tick_config.target_video_present_queue,
        tick_config.max_video_present_queue,
        "video present queue watermarks must satisfy min <= target <= max",
    )?;
    ensure_watermark_order(
        tick_config.min_texture_slots_available_for_decode,
        tick_config.target_texture_slots_available_for_decode,
        usize::MAX,
        "texture-slot watermarks must satisfy min <= target",
    )?;
    ensure_non_zero(
        tick_config.max_pending_video_packets,
        PlayerRuntimeSettingId::VideoDecoderPacketChannelFrames,
    )?;
    ensure_non_zero(
        tick_config.decoder_ready_queue_frames,
        PlayerRuntimeSettingId::VideoDecoderReadyQueueFrames,
    )?;
    ensure_non_zero(
        tick_config.max_video_packets_sent_per_tick,
        PlayerRuntimeSettingId::VideoSchedulerVideoPacketsPerTick,
    )?;
    ensure_non_zero(
        tick_config.max_decoded_video_frames_drained_per_tick,
        PlayerRuntimeSettingId::VideoSchedulerDecodedFramesPerTick,
    )?;
    ensure_duration_order(
        tick_config.target_video_decode_ahead,
        tick_config.max_video_decode_ahead,
        "video decode-ahead must satisfy target <= max",
    )?;
    ensure_positive_duration(
        tick_config.adaptive_catch_up_time_budget,
        PlayerRuntimeSettingId::VideoSchedulerCatchUpBudgetMs,
    )?;
    ensure_positive_duration(
        tick_config.seek_fast_preroll_time_budget,
        PlayerRuntimeSettingId::PlayerSeekFastPrerollBudgetMs,
    )?;
    ensure_non_zero(
        tick_config.seek_fast_preroll_video_packet_burst,
        PlayerRuntimeSettingId::PlayerSeekFastPrerollVideoPacketBurst,
    )?;
    ensure_non_negative_finite(
        tick_config.audio_buffer_high_water_mark_ms,
        PlayerRuntimeSettingId::AudioBufferTargetMs,
    )?;
    ensure_non_negative_finite(
        tick_config.audio_demux_low_water_mark_ms,
        PlayerRuntimeSettingId::AudioBufferTargetMs,
    )?;
    ensure_non_negative_finite(
        tick_config.audio_preroll_target_ms,
        PlayerRuntimeSettingId::PlayerSeekResumeAudioMinBufferMs,
    )?;
    ensure_positive_duration(
        tick_config.seek_commit_timeout,
        PlayerRuntimeSettingId::PlayerSeekCommitTimeoutMs,
    )?;
    ensure_non_negative_finite(
        tick_config.seek_resume_audio_min_buffer_ms,
        PlayerRuntimeSettingId::PlayerSeekResumeAudioMinBufferMs,
    )?;
    ensure_positive_duration(
        tick_config.seek_resume_audio_gate_timeout,
        PlayerRuntimeSettingId::PlayerSeekResumeAudioGateTimeoutMs,
    )?;
    ensure_non_zero(
        tick_config.seek_resume_video_min_ready_frames,
        PlayerRuntimeSettingId::PlayerSeekResumeVideoMinReadyFrames,
    )?;
    ensure_non_negative_finite(
        tick_config.video_present_lead_frames,
        PlayerRuntimeSettingId::PlayerTickConfig,
    )?;
    ensure_non_negative_finite(
        tick_config.video_present_window_frames,
        PlayerRuntimeSettingId::PlayerTickConfig,
    )?;
    ensure_non_negative_finite(
        tick_config.video_late_drop_grace_frames,
        PlayerRuntimeSettingId::PlayerTickConfig,
    )?;

    Ok(())
}

fn collect_or_default<I>(
    affected_settings: I,
    default_settings: &[PlayerRuntimeSettingId],
) -> Vec<PlayerRuntimeSettingId>
where
    I: IntoIterator<Item = PlayerRuntimeSettingId>,
{
    let collected = affected_settings.into_iter().collect::<Vec<_>>();
    if collected.is_empty() {
        default_settings.to_vec()
    } else {
        collected
    }
}

fn ensure_non_zero(value: usize, setting_id: PlayerRuntimeSettingId) -> Result<(), String> {
    if value == 0 {
        return Err(format!("{} must be greater than zero", setting_id.as_str()));
    }

    Ok(())
}

fn ensure_watermark_order(
    minimum: usize,
    target: usize,
    maximum: usize,
    message: &'static str,
) -> Result<(), String> {
    if minimum > target || target > maximum {
        return Err(message.to_string());
    }

    Ok(())
}

fn ensure_duration_order(
    target: Duration,
    maximum: Duration,
    message: &'static str,
) -> Result<(), String> {
    if target > maximum {
        return Err(message.to_string());
    }

    Ok(())
}

fn ensure_positive_duration(
    value: Duration,
    setting_id: PlayerRuntimeSettingId,
) -> Result<(), String> {
    if value.is_zero() {
        return Err(format!("{} must be greater than zero", setting_id.as_str()));
    }

    Ok(())
}

fn ensure_non_negative_finite(
    value: f64,
    setting_id: PlayerRuntimeSettingId,
) -> Result<(), String> {
    if !value.is_finite() || value < 0.0 {
        return Err(format!(
            "{} must be a finite non-negative value, got {value}",
            setting_id.as_str()
        ));
    }

    Ok(())
}
