use std::path::{Path, PathBuf};
use std::time::Duration;

use media_core::TrackInfo;
use webm_demux::{DemuxSeekability, Demuxer, DemuxerOptions};

/// Подготовленный media source, который уже открыт, но ещё не применён к state machine.
pub(crate) struct OpenedMedia {
    /// Источник media: локальный файл или внешний streaming label.
    pub source: OpenedMediaSource,

    /// Demuxer, готовый к чтению packets текущего media.
    pub demuxer: Box<dyn Demuxer + Send>,

    /// Tracks, считанные из demuxer metadata на этапе opening.
    pub tracks: Vec<TrackInfo>,

    /// Duration container-а, если demuxer смог её определить.
    pub duration: Option<Duration>,

    /// Seekability container-а, которую session публикует в timeline snapshot.
    pub seekability: DemuxSeekability,
}

impl OpenedMedia {
    /// Открывает локальный файл и возвращает только подготовленные media данные.
    ///
    /// State transition (`Opening`, `Failed`, autoplay) остаётся в `PlayerSession`,
    /// чтобы IO/opening слой не менял playback state напрямую.
    pub(crate) fn open_local_file(
        path: &Path,
        demuxer_options: DemuxerOptions,
    ) -> anyhow::Result<Self> {
        let demuxer = webm_demux::SymphoniaDemuxer::from_file_with_options(path, demuxer_options)?;
        Ok(Self::from_open_demuxer(
            OpenedMediaSource::LocalFile(path.to_path_buf()),
            Box::new(demuxer),
        ))
    }

    /// Оборачивает demuxer, который внешний service layer уже открыл заранее.
    pub(crate) fn from_external_demuxer(label: String, demuxer: Box<dyn Demuxer + Send>) -> Self {
        Self::from_open_demuxer(OpenedMediaSource::ExternalLabel(label), demuxer)
    }

    /// Снимает metadata с demuxer один раз и сохраняет её рядом с owned demuxer.
    fn from_open_demuxer(source: OpenedMediaSource, demuxer: Box<dyn Demuxer + Send>) -> Self {
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
pub(crate) enum OpenedMediaSource {
    /// Media открыт из локальной файловой системы.
    LocalFile(PathBuf),

    /// Media пришёл из внешнего resolver-а как уже готовый streaming demuxer.
    ExternalLabel(String),
}

impl OpenedMediaSource {
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
