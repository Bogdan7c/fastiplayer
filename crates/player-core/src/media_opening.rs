mod prefetched_demuxer;

use std::collections::VecDeque;
use std::path::PathBuf;
use std::time::Duration;

use media_core::{
    DemuxReadEvent, DemuxSeekability, Demuxer, MediaMetadata, MediaTime, TrackInfo, TrackKind,
};

use self::prefetched_demuxer::PrefetchedDemuxer;

use crate::{MediaPlaybackWindow, MediaSource};

/// Безопасная, UI-ready информация об источнике без reopen credentials.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaSourceInfo {
    /// Нейтральный вид источника.
    pub kind: MediaSourceKind,
    /// Отображаемая локация; URL очищается от query/fragment на service boundary.
    pub display_location: String,
    /// Размер только когда opener уже знает его без дополнительного I/O.
    pub size_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaSourceKind {
    LocalFile,
    Remote,
    External,
}

pub(crate) struct PreparedMediaSlots {
    pub(crate) demuxer: Box<dyn Demuxer + Send>,
    pub(crate) file_path: Option<PathBuf>,
    pub(crate) source_label: Option<String>,
    pub(crate) tracks: Vec<TrackInfo>,
    pub(crate) source_info: MediaSourceInfo,
    pub(crate) playback_window: Option<MediaPlaybackWindow>,
}

/// Cold staged-prefetch state хранится за indirection, чтобы не раздувать worker commands.
struct PreparedMediaPrefetchState {
    /// Metadata snapshot до первого prefetch event-а.
    initial_media_metadata: Option<MediaMetadata>,

    /// Demux events, прочитанные preflight-ом до atomic install candidate-а.
    events: VecDeque<DemuxReadEvent>,
}

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

    /// Optional absolute playback window, которое player публикует как relative timeline.
    playback_window: Option<MediaPlaybackWindow>,

    /// Cold replay state создаётся только если exact preflight действительно читает demuxer.
    prefetch_state: Option<Box<PreparedMediaPrefetchState>>,

    /// Typed source snapshot, подготовленный opener-слоем для Info.
    pub(crate) source_info: MediaSourceInfo,
}

impl PreparedMedia {
    /// Упорядочивает только video-track candidates по explicit codec policy.
    ///
    /// Demuxer, track ids и non-video порядок не меняются; default selection выше
    /// видит preferred video track первым, а packet routing остаётся id-based.
    #[must_use]
    pub fn with_preferred_video_codecs(
        mut self,
        preferred_codecs: &[codec_core::VideoCodec],
    ) -> Self {
        self.tracks.sort_by_key(|track| {
            if track.kind != TrackKind::Video {
                return usize::MAX;
            }

            codec_core::VideoCodec::from_container_codec_id(&track.codec_id)
                .and_then(|codec| {
                    preferred_codecs
                        .iter()
                        .position(|preferred| *preferred == codec)
                })
                .unwrap_or(preferred_codecs.len())
        });
        self
    }

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

    /// Восстанавливает neutral source только для player lifecycle event-а.
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

    /// Возвращает neutral playback window без раскрытия demuxer ownership.
    #[must_use]
    pub const fn playback_window(&self) -> Option<MediaPlaybackWindow> {
        self.playback_window
    }

    /// Устанавливает уже проверенный neutral playback window на prepared source.
    #[must_use]
    pub fn with_playback_window(mut self, playback_window: MediaPlaybackWindow) -> Self {
        self.playback_window = Some(playback_window);
        self
    }

    /// Возвращает диагностику отсутствующего video track-а для текущего типа source.
    #[must_use]
    pub(crate) fn missing_video_track_message(&self) -> &'static str {
        self.source.missing_video_track_message()
    }

    /// Читает и сохраняет следующий demux event для staged video probe.
    ///
    /// Event клонируется только на нейтральной boundary: encoded payload использует
    /// reference-counted bytes, а оригинал остаётся в replay queue для будущего media.
    pub(crate) fn prefetch_next_event_for_video_probe(&mut self) -> anyhow::Result<DemuxReadEvent> {
        let event = self.demuxer.next_event()?;
        if matches!(event, DemuxReadEvent::TemporarilyUnavailable(_)) {
            return Ok(event);
        }
        if self.prefetch_state.is_none() {
            self.prefetch_state = Some(Box::new(PreparedMediaPrefetchState {
                initial_media_metadata: self.demuxer.media_metadata(),
                events: VecDeque::new(),
            }));
        }
        let prefetch_state = self
            .prefetch_state
            .as_mut()
            .expect("prefetch state initialized before demux read");
        prefetch_state.events.push_back(event.clone());
        Ok(event)
    }

    /// Проверяет и позиционирует candidate demuxer до strong-install Ready barrier.
    pub(crate) fn prepare_playback_window(&mut self) -> anyhow::Result<()> {
        let Some(playback_window) = self.playback_window else {
            return Ok(());
        };
        playback_window.validate_source_duration(self.duration)?;
        if playback_window.start() == MediaTime::ZERO {
            return Ok(());
        }
        if !matches!(self.seekability, DemuxSeekability::Seekable) {
            anyhow::bail!("playback window с ненулевым start требует seekable source");
        }

        let seek_result = self.demuxer.seek(playback_window.start().as_duration())?;
        if seek_result.actual_position > playback_window.start() {
            anyhow::bail!("demuxer positioned playback window after its requested absolute start");
        }

        // Video preflight мог прочитать packets до window seek. После reposition
        // они принадлежат старой demux position и не должны попасть в новый pipeline.
        self.prefetch_state = None;
        Ok(())
    }

    /// Разбирает prepared media на slots, которые `PlaybackPipeline` устанавливает как владелец.
    pub(crate) fn into_pipeline_slots(self) -> PreparedMediaSlots {
        let file_path = self.source.pipeline_file_path();
        let source_label = self.source.pipeline_source_label();

        let demuxer = match self.prefetch_state {
            Some(prefetch_state) if !prefetch_state.events.is_empty() => {
                Box::new(PrefetchedDemuxer::new(
                    self.demuxer,
                    self.tracks.clone(),
                    self.duration,
                    self.seekability,
                    prefetch_state.initial_media_metadata,
                    prefetch_state.events,
                ))
            }
            Some(_) | None => self.demuxer,
        };

        PreparedMediaSlots {
            demuxer,
            file_path,
            source_label,
            tracks: self.tracks,
            source_info: self.source_info,
            playback_window: self.playback_window,
        }
    }

    /// Снимает metadata с demuxer один раз и сохраняет её рядом с owned demuxer.
    fn from_open_demuxer(source: PreparedMediaSource, demuxer: Box<dyn Demuxer + Send>) -> Self {
        let tracks = demuxer.tracks().to_vec();
        let duration = demuxer.duration();
        let seekability = demuxer.seekability();
        let source_info = source.source_info();

        Self {
            source,
            demuxer,
            tracks,
            duration,
            seekability,
            playback_window: None,
            prefetch_state: None,
            source_info,
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
    fn source_info(&self) -> MediaSourceInfo {
        match self {
            Self::LocalFile(path) => MediaSourceInfo {
                kind: MediaSourceKind::LocalFile,
                display_location: path.display().to_string(),
                size_bytes: std::fs::metadata(path).map(|metadata| metadata.len()).map_err(|error| {
                    tracing::warn!(path = %path.display(), error = %error, "Не удалось прочитать размер media source для Info");
                }).ok(),
            },
            Self::ExternalLabel(label) => MediaSourceInfo {
                kind: if label.starts_with("http://") || label.starts_with("https://") { MediaSourceKind::Remote } else { MediaSourceKind::External },
                display_location: label.split(['?', '#']).next().unwrap_or(label).to_owned(),
                size_bytes: None,
            },
        }
    }

    /// Возвращает label для snapshot/event слоя.
    pub(crate) fn display_label(&self) -> String {
        match self {
            Self::LocalFile(path) => path.display().to_string(),
            Self::ExternalLabel(label) => label.clone(),
        }
    }

    /// Преобразует prepared source в существующий neutral player command contract.
    fn media_source(&self) -> MediaSource {
        match self {
            Self::LocalFile(path) => MediaSource::LocalFile(path.clone()),
            Self::ExternalLabel(label) => MediaSource::ExternalLabel(label.clone()),
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
