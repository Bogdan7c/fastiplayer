use std::path::PathBuf;
use std::time::Duration;

use media_core::{MediaTime, TrackId};

/// Идентификатор качества или варианта потока.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct QualityId(String);

impl QualityId {
    /// Создаёт opaque ID качества без привязки к конкретному сервису.
    #[must_use]
    pub fn new(raw_quality_id: impl Into<String>) -> Self {
        Self(raw_quality_id.into())
    }

    /// Возвращает строковое представление для UI и сервисных адаптеров.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Источник media, который пользователь просит открыть.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSource {
    /// Локальный файл, выбранный пользователем или переданный через CLI.
    LocalFile(PathBuf),

    /// Сетевой URL, который позже обработает `source-core`.
    Url(String),

    /// Уже подготовленный внешний источник с человекочитаемой меткой.
    ExternalLabel(String),
}

impl MediaSource {
    /// Возвращает безопасную метку источника для snapshot'а и событий.
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::LocalFile(path) => path.display().to_string(),
            Self::Url(url) => url.clone(),
            Self::ExternalLabel(label) => label.clone(),
        }
    }
}

/// Запрос на открытие media без владения demuxer или decoder handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaOpenRequest {
    /// Источник bytes или stream manifest.
    pub source: MediaSource,

    /// Нужно ли автоматически начать воспроизведение после успешного открытия.
    pub autoplay: bool,
}

impl MediaOpenRequest {
    /// Создаёт запрос открытия media с явным autoplay-флагом.
    #[must_use]
    pub const fn new(source: MediaSource, autoplay: bool) -> Self {
        Self { source, autoplay }
    }
}

/// Политика seek-операции, которую должен выбрать scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// Seek с минимальной задержкой: demuxer стартует до target, а playback продолжается с первого свежего кадра.
    Accurate,

    /// Seek к ближайшему ключевому кадру до указанной позиции.
    KeyframeBefore,

    /// Seek к ближайшему ключевому кадру после target; пока отклоняется typed ошибкой.
    KeyframeAfter,
}

impl Default for SeekMode {
    /// По умолчанию выбираем точный seek как наиболее ожидаемое UI-поведение.
    fn default() -> Self {
        Self::Accurate
    }
}

/// Цель seek-запроса без привязки к UI control или container timestamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekTarget {
    /// Абсолютная позиция на нормализованной media timeline.
    Absolute(MediaTime),

    /// Относительный шаг от текущей позиции playback.
    Relative(Duration),
}

impl SeekTarget {
    /// Разрешает цель в абсолютную позицию для текущей timeline-позиции.
    #[must_use]
    pub fn resolve(self, current_position: MediaTime) -> MediaTime {
        match self {
            Self::Absolute(position) => position,
            Self::Relative(delta) => current_position.saturating_add(delta.into()),
        }
    }
}

/// Запрос перемотки внутри текущего media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekRequest {
    /// Целевая позиция: абсолютная media time или относительный шаг.
    pub target: SeekTarget,

    /// Политика точности перемотки.
    pub mode: SeekMode,
}

impl SeekRequest {
    /// Создаёт seek-запрос с точной перемоткой из legacy `Duration` позиции.
    #[must_use]
    pub const fn accurate(position: Duration) -> Self {
        Self {
            target: SeekTarget::Absolute(MediaTime::from_duration(position)),
            mode: SeekMode::Accurate,
        }
    }

    /// Создаёт seek-запрос к абсолютной media-позиции.
    #[must_use]
    pub const fn absolute(position: MediaTime) -> Self {
        Self {
            target: SeekTarget::Absolute(position),
            mode: SeekMode::Accurate,
        }
    }

    /// Создаёт seek-запрос как положительный шаг от текущей позиции.
    #[must_use]
    pub const fn relative(delta: Duration) -> Self {
        Self {
            target: SeekTarget::Relative(delta),
            mode: SeekMode::Accurate,
        }
    }
}

/// Совместимый параметр завершения interactive scrub.
///
/// Текущий player-core временно игнорирует значение policy: `EndScrub` сохраняет
/// форму public API до будущей переписи preview-пайплайна, но завершает drag обычным
/// final seek-ом.
/// Обычные exact seek-команды должны идти через `PlayerCommand::Seek`, чтобы не
/// наследовать будущую UX policy timeline-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrubCommitPolicy {
    /// Exact final target: всегда выполнить final seek в последнюю цель `UpdateScrub`.
    CommitLatestTarget,

    /// UX smooth pointer release: зафиксировать последний реально видимый preview.
    CommitVisiblePreview,
}

impl ScrubCommitPolicy {
    /// UX policy по умолчанию для отпускания pointer-а на timeline.
    ///
    /// Значение сохранено только для source/binary compatibility существующих callers.
    /// До будущей переписи preview-пайплайна session трактует оба enum-варианта одинаково.
    pub const DEFAULT_TIMELINE_RELEASE: Self = Self::CommitVisiblePreview;
}

/// Выбор качества потока для локального файла или сетевого сервиса.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum QualitySelection {
    /// Автоматический выбор на основе capability matrix и bandwidth.
    Auto,

    /// Конкретный вариант, выбранный пользователем или сервисным слоем.
    Specific(QualityId),
}

/// Команда, которую UI или внешняя интеграция отправляет player state machine.
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerCommand {
    /// Открыть новый media-источник.
    OpenMedia(MediaOpenRequest),

    /// Начать или продолжить воспроизведение.
    Play,

    /// Приостановить воспроизведение.
    Pause,

    /// Переключить состояние между play и pause.
    TogglePlayback,

    /// Выполнить exact/accurate final seek текущего media.
    ///
    /// Эта команда не использует `ScrubCommitPolicy` и остаётся route-ом для keyboard seek,
    /// external/MPRIS seek, будущего chapter seek и любого отделённого exact click-to-seek.
    Seek(SeekRequest),

    /// Начать compatibility scrub без немедленного seek-а.
    BeginScrub,

    /// Запомнить latest target compatibility scrub-а без запуска demux seek-а.
    UpdateScrub(SeekRequest),

    /// Запомнить latest target compatibility scrub-а без запуска demux preview-mode seek-а.
    ///
    /// Временный fallback трактует preview как `UpdateScrub`: target сохраняется
    /// для `EndScrub`, но player-core preview-пайплайн не стартует.
    PreviewScrub(SeekRequest),

    /// Завершить compatibility scrub обычным final seek-ом в latest target.
    ///
    /// `policy` пока сохраняется только для совместимости public API и не влияет
    /// на выбор target-а: session всегда берёт последний `UpdateScrub`/`PreviewScrub`.
    EndScrub { policy: ScrubCommitPolicy },

    /// Остановить текущий media без завершения всей session.
    Stop,

    /// Установить громкость в диапазоне `0.0..=1.0`.
    SetVolume(f32),

    /// Выбрать активный video track.
    SelectVideoTrack(TrackId),

    /// Выбрать активный audio track.
    SelectAudioTrack(TrackId),

    /// Выбрать subtitle track или отключить субтитры через `None`.
    SelectSubtitleTrack(Option<TrackId>),

    /// Выбрать качество потока.
    SelectQuality(QualitySelection),

    /// Перечитать runtime config.
    ReloadConfig,

    /// Завершить player session.
    Shutdown,
}
