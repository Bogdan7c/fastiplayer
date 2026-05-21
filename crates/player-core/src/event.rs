use std::time::Duration;

use crate::{
    MediaOpenRequest, PlaybackResumeIntent, PlaybackState, PlayerError, QualitySelection,
    ScrubCommitPolicy, SeekRequest, TrackId,
};

/// Краткое описание media после успешного открытия.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSummary {
    /// Заголовок media, если он известен.
    pub title: Option<String>,

    /// Метка источника: путь, URL или сервисное имя.
    pub source_label: String,

    /// Длительность media, если она известна.
    pub duration: Option<Duration>,
}

/// Сведения о кадре, который готов к presentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FramePresentationInfo {
    /// Opaque handle кадра на render boundary.
    pub handle: u64,

    /// Presentation timestamp кадра.
    pub pts: Duration,

    /// Номер кадра внутри текущей session.
    pub sequence_number: u64,
}

/// Состояние buffering с явной причиной.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferingState {
    /// Активен ли buffering прямо сейчас.
    pub active: bool,

    /// Человекочитаемая причина для diagnostics.
    pub reason: Option<String>,
}

/// Сведения о target frame, который уже стал текущим presented frame после seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekTargetFramePresentation {
    /// Пользовательская target-позиция seek transaction-а.
    pub target_position: Duration,

    /// PTS кадра, который реально показан после seek.
    pub frame_pts: Duration,
}

/// Сведения о закрытом seek transaction-е.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekCommitInfo {
    /// Пользовательская target-позиция seek transaction-а.
    pub target_position: Duration,

    /// Фактическая container-позиция, на которую demuxer принял seek.
    pub actual_position: Duration,

    /// Playback intent, который session применила после закрытия gates.
    pub resume_intent: PlaybackResumeIntent,
}

/// Сведения о restart-е audio output после seek.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekAudioResumeInfo {
    /// Target-позиция seek-а, для которого audio output был запущен.
    pub target_position: Duration,
}

/// Источник позиции, выбранной при release interactive scrub-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubReleaseCommitSource {
    /// Коммитится последний preview frame, который реально был показан.
    VisiblePreview,

    /// Коммитится последняя pointer target-позиция, потому что visible frame непригоден.
    LatestTarget,

    /// Уже существующий preview transaction переиспользован как final commit.
    PromotedPreview,
}

/// Диагностика release policy для одного `EndScrub`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrubReleaseCommitInfo {
    /// Release policy, выбранная UI/config-слоем.
    pub release_policy: ScrubCommitPolicy,

    /// Откуда взята финальная release-позиция.
    pub committed_from: ScrubReleaseCommitSource,

    /// Позиция, которую release path выбрал как commit target.
    pub committed_position: Duration,

    /// Последняя pointer target-позиция на момент release.
    pub latest_target: Duration,
}

/// Краткая сводка возможностей системы.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilitySummary {
    /// Текстовая сводка для UI или логов.
    pub summary: String,
}

/// Событие, которое player-core отдаёт shell-слою после обработки команд или tick.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerEvent {
    /// Запрос открытия media принят state machine.
    MediaOpenRequested(MediaOpenRequest),

    /// Media успешно открыто и базовые metadata доступны.
    MediaOpened(MediaSummary),

    /// Состояние воспроизведения изменилось.
    PlaybackStateChanged(PlaybackState),

    /// Позиция воспроизведения изменилась.
    PositionChanged(Duration),

    /// Seek-запрос принят state machine.
    SeekRequested(SeekRequest),

    /// Target frame финального seek-а уже показан независимо от audio gate-а.
    SeekTargetFramePresented(SeekTargetFramePresentation),

    /// Seek transaction закрыт после готовых gates или разрешённого soft fallback-а.
    SeekCommitted(SeekCommitInfo),

    /// Audio output запущен после закрытия seek transaction-а.
    AudioResumedAfterSeek(SeekAudioResumeInfo),

    /// Interactive scrub release выбрал объяснимую commit-позицию.
    ScrubReleaseCommitted(ScrubReleaseCommitInfo),

    /// Кадр готов к presentation.
    VideoFrameReady(FramePresentationInfo),

    /// Buffering начался или закончился.
    BufferingStateChanged(BufferingState),

    /// Capability probing завершён.
    CapabilityScanCompleted(CapabilitySummary),

    /// Video track выбран.
    VideoTrackSelected(TrackId),

    /// Audio track выбран.
    AudioTrackSelected(TrackId),

    /// Subtitle track выбран или отключён.
    SubtitleTrackSelected(Option<TrackId>),

    /// Качество потока выбрано.
    QualitySelectionChanged(QualitySelection),

    /// Runtime config нужно перечитать.
    ConfigReloadRequested,

    /// Session получила shutdown-запрос.
    ShutdownRequested,

    /// Восстановимая ошибка, после которой player может продолжать работу.
    RecoverableError(PlayerError),

    /// Fatal error текущего media или runtime pipeline.
    FatalError(PlayerError),
}
