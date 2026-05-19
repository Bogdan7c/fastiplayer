use std::path::PathBuf;
use std::time::Duration;

use media_core::{DemuxSeekability, Demuxer, TrackInfo};

use crate::MediaSource;

/// Подготовленный media source, который уже открыт за границей `player-core`.
pub struct PreparedMedia {
    /// Источник media: локальный файл или внешний streaming label.
    pub(crate) source: PreparedMediaSource,

    /// Demuxer, готовый к чтению packets текущего media.
    pub(crate) demuxer: Box<dyn Demuxer + Send>,

    /// Tracks, считанные из demuxer metadata на этапе opening.
    pub(crate) tracks: Vec<TrackInfo>,

    /// Duration container-а, если demuxer смог её определить.
    pub(crate) duration: Option<Duration>,

    /// Seekability container-а, которую session публикует в timeline snapshot.
    pub(crate) seekability: DemuxSeekability,
}

impl PreparedMedia {
    /// Создаёт prepared-media contract для локального файла, уже открытого shell/adapter слоем.
    #[must_use]
    pub fn from_local_file(path: impl Into<PathBuf>, demuxer: Box<dyn Demuxer + Send>) -> Self {
        Self::from_open_demuxer(PreparedMediaSource::LocalFile(path.into()), demuxer)
    }

    /// Оборачивает demuxer, который внешний service layer уже открыл заранее.
    #[must_use]
    pub fn from_external_label(label: impl Into<String>, demuxer: Box<dyn Demuxer + Send>) -> Self {
        Self::from_open_demuxer(PreparedMediaSource::ExternalLabel(label.into()), demuxer)
    }

    /// Возвращает user-facing label source-а без передачи владения demuxer-ом.
    #[must_use]
    pub fn source_label(&self) -> String {
        self.source.display_label()
    }

    /// Возвращает public open-request source для session state machine.
    #[must_use]
    pub(crate) fn media_source(&self) -> MediaSource {
        self.source.media_source()
    }

    /// Возвращает title, который UI может показать как имя media.
    #[must_use]
    pub fn media_title(&self) -> Option<String> {
        self.source.media_title()
    }

    /// Возвращает immutable snapshot tracks, снятый во время подготовки media.
    #[must_use]
    pub fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    /// Возвращает количество tracks без передачи доступа к storage.
    #[must_use]
    pub(crate) fn track_count(&self) -> usize {
        self.tracks.len()
    }

    /// Возвращает duration, снятую во время подготовки media.
    #[must_use]
    pub const fn duration(&self) -> Option<Duration> {
        self.duration
    }

    /// Возвращает seekability, снятую во время подготовки media.
    #[must_use]
    pub const fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    /// Возвращает диагностику отсутствующего video track-а для текущего типа source.
    #[must_use]
    pub(crate) fn missing_video_track_message(&self) -> &'static str {
        self.source.missing_video_track_message()
    }

    /// Разбирает prepared media на slots, которые `PlaybackPipeline` устанавливает как владелец.
    pub(crate) fn into_pipeline_slots(
        self,
    ) -> (
        Box<dyn Demuxer + Send>,
        Option<PathBuf>,
        Option<String>,
        Vec<TrackInfo>,
    ) {
        let file_path = self.source.pipeline_file_path();
        let source_label = self.source.pipeline_source_label();

        (self.demuxer, file_path, source_label, self.tracks)
    }

    /// Снимает metadata с demuxer один раз и сохраняет её рядом с owned demuxer.
    fn from_open_demuxer(source: PreparedMediaSource, demuxer: Box<dyn Demuxer + Send>) -> Self {
        let tracks = demuxer.tracks().to_vec();
        let duration = demuxer.duration();
        let seekability = demuxer.seekability();

        Self {
            source,
            demuxer,
            tracks,
            duration,
            seekability,
        }
    }
}

/// User-visible identity открытого media source без доступа к demuxer internals.
pub enum PreparedMediaSource {
    /// Media открыт из локальной файловой системы.
    LocalFile(PathBuf),

    /// Media пришёл из внешнего resolver-а как уже готовый streaming demuxer.
    ExternalLabel(String),
}

impl PreparedMediaSource {
    /// Возвращает public open-request source для session state machine.
    pub(crate) fn media_source(&self) -> MediaSource {
        match self {
            Self::LocalFile(path) => MediaSource::LocalFile(path.clone()),
            Self::ExternalLabel(label) => MediaSource::ExternalLabel(label.clone()),
        }
    }

    /// Возвращает label для snapshot/event слоя.
    pub(crate) fn display_label(&self) -> String {
        match self {
            Self::LocalFile(path) => path.display().to_string(),
            Self::ExternalLabel(label) => label.clone(),
        }
    }

    /// Возвращает title, который UI показывает как имя media.
    pub(crate) fn media_title(&self) -> Option<String> {
        match self {
            Self::LocalFile(path) => path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(ToOwned::to_owned),
            Self::ExternalLabel(label) => Some(label.clone()),
        }
    }

    /// Возвращает путь для pipeline или `None`, если source не локальный.
    pub(crate) fn pipeline_file_path(&self) -> Option<PathBuf> {
        match self {
            Self::LocalFile(path) => Some(path.clone()),
            Self::ExternalLabel(_) => None,
        }
    }

    /// Возвращает streaming label для pipeline или `None`, если source локальный.
    pub(crate) fn pipeline_source_label(&self) -> Option<String> {
        match self {
            Self::LocalFile(_) => None,
            Self::ExternalLabel(label) => Some(label.clone()),
        }
    }

    /// Диагностика выбора video track отличается для файла и external demuxer.
    pub(crate) fn missing_video_track_message(&self) -> &'static str {
        match self {
            Self::LocalFile(_) => "Поддерживаемый video track не найден",
            Self::ExternalLabel(_) => "Поддерживаемый video track не найден в streaming demuxer",
        }
    }
}
