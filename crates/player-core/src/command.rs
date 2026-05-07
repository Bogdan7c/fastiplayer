use std::path::PathBuf;
use std::time::Duration;

/// Идентификатор media-трека внутри текущего контейнера.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TrackId(u32);

impl TrackId {
    /// Создаёт typed wrapper вокруг числового ID трека.
    #[must_use]
    pub const fn new(raw_track_id: u32) -> Self {
        Self(raw_track_id)
    }

    /// Возвращает исходный ID трека для адаптеров старого pipeline.
    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

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

/// Точность seek-операции, которую должен выбрать будущий scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SeekMode {
    /// Точный seek, если контейнер и codec pipeline это позволяют.
    Accurate,

    /// Seek к ближайшему ключевому кадру до указанной позиции.
    KeyframeBefore,

    /// Seek к ближайшему ключевому кадру после указанной позиции.
    KeyframeAfter,
}

impl Default for SeekMode {
    /// По умолчанию выбираем точный seek как наиболее ожидаемое UI-поведение.
    fn default() -> Self {
        Self::Accurate
    }
}

/// Запрос перемотки внутри текущего media.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekRequest {
    /// Целевая media-позиция от начала файла.
    pub position: Duration,

    /// Политика точности перемотки.
    pub mode: SeekMode,
}

impl SeekRequest {
    /// Создаёт seek-запрос с точной перемоткой.
    #[must_use]
    pub const fn accurate(position: Duration) -> Self {
        Self {
            position,
            mode: SeekMode::Accurate,
        }
    }
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

    /// Перемотать текущий media.
    Seek(SeekRequest),

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
