use std::time::Duration;

use crate::{MediaOpenRequest, PlaybackState, PlayerError, QualitySelection, SeekRequest, TrackId};

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
