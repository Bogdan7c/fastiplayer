use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaTime, Packet as OurPacket, PacketKeyframe,
    TimelineNotSeekableReason, TrackId, TrackInfo, TrackKind, TrackTimestamp,
};
use source_core::{
    ByteSource, CancellationToken, Seekability as SourceSeekability, SourceError, SourceResult,
};
use tracing::{debug, info, trace, warn};

use crate::byte_source::ByteSourceMediaSource;
use crate::error::DemuxError;
use crate::matroska_metadata::{
    MatroskaVideoTrack, extract_video_tracks_from_file, scan_video_tracks_from_bytes,
};
use crate::options::DemuxerOptions;
use crate::packet_mapper::{PacketConvertError, convert_packet};
use crate::seek_mapper::{
    preferred_seek_track_id, seeked_to_timeline_result, symphonia_seek_error_to_demux_error,
    symphonia_seek_mode, symphonia_seek_target,
};
use crate::symphonia_api::{
    self, FormatReaderBox, Hint, MediaSourceStream, ReadOnlySource, SeekErrorKind, SeekedTo,
    SymphoniaError, SymphoniaSeekMode,
};
use crate::track_mapper::{TrackEntry, map_tracks, tracks_may_need_matroska_video_metadata};

/// Верхняя граница prefix scan-а для seekable byte source-ов.
const MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Более короткая граница для unseekable stream, чтобы open не ждал большой network prefix.
const MATROSKA_STREAM_SCAN_LIMIT_BYTES: usize = 256 * 1024;

/// Лимит повторов для восстановления before-or-at-target semantics после backend overshoot.
const DECODE_POINT_BEFORE_MAX_RETRIES: usize = 6;

/// Минимальный отступ назад, чтобы retry не попал в ту же after-target packet boundary.
const DECODE_POINT_BEFORE_RETRY_MARGIN: Duration = Duration::from_millis(1);

/// Первый шаг назад для поиска рабочей позиции, когда Symphonia считает in-range цель концом stream-а.
const IN_RANGE_OUT_OF_RANGE_SEEK_INITIAL_RETRY_OFFSET: Duration = Duration::from_millis(10);

/// Ограничивает число дорогих reprobe/seek попыток при повреждённых или неполных Matroska cues.
const IN_RANGE_OUT_OF_RANGE_SEEK_MAX_EXPONENTIAL_RETRIES: usize = 32;

/// Уточнение ближе миллисекунды не даёт практической пользы для текущих container timebase-ов.
const IN_RANGE_OUT_OF_RANGE_SEEK_REFINEMENT_EPSILON: Duration = Duration::from_millis(1);

/// Ограничивает binary refinement после того, как найден первый рабочий timestamp перед целью.
const IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES: usize = 10;

/// Demuxer на базе Symphonia для media containers, которые поддерживает workspace dependency.
pub struct SymphoniaDemuxer {
    format: Option<FormatReaderBox<'static>>,
    probe_hint: Hint,
    source_label: String,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    matroska_video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
    seekability: DemuxSeekability,
    options: DemuxerOptions,
    pending_events: VecDeque<DemuxReadEvent>,
    end_of_stream_reached: bool,
}

/// Снимок track state-а, который rebuild может проверить перед заменой reader-а.
struct SymphoniaTrackState {
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    track_duration: Option<Duration>,
    media_info_duration: Option<Duration>,
}

impl SymphoniaDemuxer {
    pub fn from_file(path: &Path) -> Result<Self, DemuxError> {
        Self::from_file_with_options(path, DemuxerOptions::default())
    }

    pub fn from_file_with_options(
        path: &Path,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        if !path.exists() {
            return Err(DemuxError::FileNotFound(path.to_path_buf()));
        }

        let file = File::open(path)?;
        let media_source_stream = MediaSourceStream::new(Box::new(file), Default::default());
        let hint = symphonia_api::hint_from_path(path);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;
        let video_tracks_by_track = extract_video_tracks_from_file_if_needed(path, format.tracks());

        Self::from_format_reader_with_probe_context(
            format,
            hint,
            &path.display().to_string(),
            video_tracks_by_track,
            DemuxSeekability::Seekable,
            options,
        )
    }

    /// Открывает media stream из потокового reader-а без seek.
    pub fn from_stream<R>(reader: R, extension_hint: &str, label: &str) -> Result<Self, DemuxError>
    where
        R: Read + Send + Sync + 'static,
    {
        Self::from_stream_with_options(reader, extension_hint, label, DemuxerOptions::default())
    }

    /// Открывает media stream из потокового reader-а без seek с явной fail-safe политикой.
    pub fn from_stream_with_options<R>(
        reader: R,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        R: Read + Send + Sync + 'static,
    {
        let (media_source_stream, video_tracks_by_track) =
            if extension_may_have_matroska_video_metadata(extension_hint) {
                let mut reader = reader;
                let (stream_prefix, video_tracks_by_track) = read_stream_prefix(&mut reader)?;
                let reader = io::Cursor::new(stream_prefix).chain(reader);
                let media_source = ReadOnlySource::new(reader);
                (
                    MediaSourceStream::new(Box::new(media_source), Default::default()),
                    video_tracks_by_track,
                )
            } else {
                trace!(
                    source = %label,
                    extension_hint,
                    "Matroska video metadata pre-scan skipped for non-Matroska stream"
                );
                let media_source = ReadOnlySource::new(reader);
                (
                    MediaSourceStream::new(Box::new(media_source), Default::default()),
                    HashMap::new(),
                )
            };

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader_with_probe_context(
            format,
            hint,
            label,
            video_tracks_by_track,
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
            options,
        )
    }

    /// Открывает media container из нейтрального seekable byte source-а.
    pub fn from_byte_source<S>(
        source: S,
        extension_hint: &str,
        label: &str,
    ) -> Result<Self, DemuxError>
    where
        S: ByteSource + 'static,
    {
        Self::from_byte_source_with_options(
            source,
            extension_hint,
            label,
            DemuxerOptions::default(),
        )
    }

    /// Открывает media container из нейтрального byte source-а с явной fail-safe политикой.
    pub fn from_byte_source_with_options<S>(
        mut source: S,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        S: ByteSource + 'static,
    {
        let source_seekability = source.seekability();
        let demux_seekability = source_seekability_to_demux_seekability(source_seekability);
        let video_tracks_by_track = if extension_may_have_matroska_video_metadata(extension_hint) {
            extract_video_tracks_from_byte_source(&mut source, label)?
        } else {
            trace!(
                source = %label,
                extension_hint,
                "Matroska video metadata pre-scan skipped for non-Matroska byte source"
            );
            HashMap::new()
        };
        let media_source = ByteSourceMediaSource::new(Box::new(source));
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader_with_probe_context(
            format,
            hint,
            label,
            video_tracks_by_track,
            demux_seekability,
            options,
        )
    }

    /// Собирает metadata и track map из готового Symphonia format reader.
    #[cfg(test)]
    fn from_format_reader(
        format: FormatReaderBox<'static>,
        label: &str,
        video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        Self::from_format_reader_with_probe_context(
            format,
            Hint::default(),
            label,
            video_tracks_by_track,
            seekability,
            options,
        )
    }

    /// Собирает metadata и track map, сохраняя context для будущего controlled reprobe.
    fn from_format_reader_with_probe_context(
        mut format: FormatReaderBox<'static>,
        probe_hint: Hint,
        label: &str,
        video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let symphonia_metadata = summarize_symphonia_format_metadata(&mut format);
        let track_state = track_state_from_format_reader(&format, &video_tracks_by_track);

        info!(
            source = %label,
            tracks = track_state.tracks.len(),
            duration = ?track_state.duration,
            track_duration = ?track_state.track_duration,
            media_info_duration = ?track_state.media_info_duration,
            attachments = symphonia_metadata.attachments,
            chapters = symphonia_metadata.has_chapters,
            metadata_revision = symphonia_metadata.has_metadata_revision,
            "Symphonia media source открыт"
        );

        Ok(Self {
            format: Some(format),
            probe_hint,
            source_label: label.to_owned(),
            tracks: track_state.tracks,
            duration: track_state.duration,
            track_map: track_state.track_map,
            matroska_video_tracks_by_track: video_tracks_by_track,
            seekability,
            options,
            pending_events: VecDeque::new(),
            end_of_stream_reached: false,
        })
    }

    /// Test-only constructor для проверок composite demuxer-ов поверх настоящей Symphonia boundary.
    #[cfg(test)]
    pub(crate) fn from_test_format_reader(
        format: FormatReaderBox<'static>,
        label: &str,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        Self::from_format_reader(format, label, HashMap::new(), seekability, options)
    }

    /// Возвращает активный Symphonia reader; `None` допустим только внутри consuming rebuild-а.
    fn format_mut(&mut self, operation: &'static str) -> Result<&mut FormatReaderBox<'static>> {
        self.format
            .as_mut()
            .ok_or(DemuxError::ReaderUnavailable { operation }.into())
    }

    /// Забирает reader для `FormatReader::into_inner()`, не раскрывая storage наружу.
    fn take_format(
        &mut self,
        operation: &'static str,
    ) -> Result<FormatReaderBox<'static>, DemuxError> {
        self.format
            .take()
            .ok_or(DemuxError::ReaderUnavailable { operation })
    }

    /// Источник можно re-probe только если source/container stack честно объявлен seekable.
    fn can_reprobe_current_source(&self) -> bool {
        matches!(self.seekability, DemuxSeekability::Seekable)
    }

    /// Забирает только lifecycle pending events, очищая packet prebuffer после нового seek-а.
    fn take_pending_lifecycle_events(&mut self) -> VecDeque<DemuxReadEvent> {
        self.pending_events
            .drain(..)
            .filter(|event| matches!(event, DemuxReadEvent::TracksChanged(_)))
            .collect()
    }

    /// Возвращает сохранённые lifecycle events перед уже существующим post-seek prebuffer-ом.
    fn prepend_pending_events(&mut self, retained_events: VecDeque<DemuxReadEvent>) {
        if retained_events.is_empty() {
            return;
        }

        let current_pending_events = std::mem::take(&mut self.pending_events);
        self.pending_events =
            prepend_retained_lifecycle_events(retained_events, current_pending_events);
    }

    /// Полностью пересоздаёт Symphonia reader из прежнего `MediaSourceStream` после EOF/reset сбоя.
    fn rebuild_format_reader_from_source_start(&mut self) -> Result<(), DemuxError> {
        if !self.can_reprobe_current_source() {
            return Err(DemuxError::SeekUnavailable(
                "source не поддерживает seek, поэтому demuxer не может выполнить reprobe"
                    .to_owned(),
            ));
        }

        let previous_tracks = self.tracks.clone();
        let previous_duration = self.duration;
        let previous_snapshot = track_layout_snapshot(&previous_tracks, previous_duration);
        let mut media_source_stream = self.take_format("reprobe")?.into_inner();

        media_source_stream.seek(SeekFrom::Start(0))?;

        let mut rebuilt_format =
            symphonia_api::probe_format_reader(&self.probe_hint, media_source_stream)?;
        let symphonia_metadata = summarize_symphonia_format_metadata(&mut rebuilt_format);
        let track_state =
            track_state_from_format_reader(&rebuilt_format, &self.matroska_video_tracks_by_track);
        let rebuilt_snapshot = track_layout_snapshot(&track_state.tracks, track_state.duration);

        if track_state.tracks != previous_tracks || track_state.duration != previous_duration {
            return Err(DemuxError::ReprobeChangedTrackLayout {
                label: self.source_label.clone(),
                before_snapshot: previous_snapshot,
                after_snapshot: rebuilt_snapshot,
            });
        }

        info!(
            source = %self.source_label,
            tracks = track_state.tracks.len(),
            duration = ?track_state.duration,
            track_duration = ?track_state.track_duration,
            media_info_duration = ?track_state.media_info_duration,
            attachments = symphonia_metadata.attachments,
            chapters = symphonia_metadata.has_chapters,
            metadata_revision = symphonia_metadata.has_metadata_revision,
            "Symphonia FormatReader пересоздан после EOF/container-state сбоя"
        );

        self.format = Some(rebuilt_format);
        self.track_map = track_state.track_map;
        self.end_of_stream_reached = false;

        Ok(())
    }

    /// Фиксирует нормальное продвижение reader-а после успешного packet read.
    fn record_successful_packet(&mut self) {
        self.end_of_stream_reached = false;
    }

    /// Перечитывает Symphonia track list после `ResetRequired` и обновляет demux boundary state.
    fn refresh_track_list_after_reset(&mut self) -> Result<DemuxTrackListUpdate> {
        let matroska_video_tracks_by_track = self.matroska_video_tracks_by_track.clone();
        let format = self.format_mut("refresh_track_list_after_reset")?;
        let track_state = track_state_from_format_reader(format, &matroska_video_tracks_by_track);

        self.tracks = track_state.tracks;
        self.duration = track_state.duration;
        self.track_map = track_state.track_map;
        self.end_of_stream_reached = false;

        info!(
            tracks = self.tracks.len(),
            duration = ?self.duration,
            track_duration = ?track_state.track_duration,
            media_info_duration = ?track_state.media_info_duration,
            "Symphonia ResetRequired обработан как обновление track list"
        );

        Ok(DemuxTrackListUpdate::new(
            self.tracks.clone(),
            self.duration,
        ))
    }

    /// Читает следующий event напрямую из Symphonia, не трогая prebuffered seek-prefix.
    fn read_next_event_from_format(&mut self) -> Result<DemuxReadEvent> {
        loop {
            let next_packet_result = self.format_mut("next_packet")?.next_packet();

            match next_packet_result {
                Ok(Some(packet)) => match convert_packet(packet, &self.track_map) {
                    Ok(our_packet) => {
                        self.record_successful_packet();
                        return Ok(DemuxReadEvent::Packet(our_packet));
                    }
                    Err(PacketConvertError::UnknownTrack { track_id }) => {
                        return Err(DemuxError::UnknownPacketTrack { track_id }.into());
                    }
                    Err(PacketConvertError::UnsupportedTrack { track_id, reason }) => {
                        trace!(
                            track_id = %track_id,
                            reason,
                            "Packet unsupported track-а пропущен"
                        );
                        continue;
                    }
                },
                Ok(None) => {
                    self.end_of_stream_reached = true;
                    return Ok(DemuxReadEvent::EndOfStream);
                }
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    self.end_of_stream_reached = true;
                    return Ok(DemuxReadEvent::EndOfStream);
                }
                Err(SymphoniaError::IoError(error)) => {
                    return Err(DemuxError::Io(error).into());
                }
                Err(SymphoniaError::ResetRequired) => {
                    let track_update = self.refresh_track_list_after_reset()?;
                    return Ok(DemuxReadEvent::TracksChanged(track_update));
                }
                Err(e) => {
                    // По контракту Symphonia ошибки `FormatReader::next_packet()`,
                    // кроме `ResetRequired`, описывают structural state reader-а.
                    return Err(DemuxError::Parse(e).into());
                }
            }
        }
    }
}

/// Читает container-level duration из Symphonia `MediaInfo`.
///
/// Symphonia 0.6 может хранить duration на media-level, даже если отдельные
/// tracks не несут `Track::duration`. Player timeline использует это только как
/// fallback, чтобы не терять seekable VOD UI при валидном seekable source-е.
fn media_info_duration(media_info: &symphonia::core::formats::MediaInfo) -> Option<Duration> {
    media_info
        .time_base
        .zip(media_info.duration)
        .map(|(time_base, duration)| {
            symphonia_api::symphonia_duration_to_duration(time_base, duration)
        })
        .filter(|duration| !duration.is_zero())
}

/// Короткая сводка того, что Symphonia 0.6 уже принесла на format-level boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SymphoniaFormatMetadataSummary {
    /// Количество attachments, которые Symphonia отдаёт через `FormatReader`.
    attachments: usize,

    /// Есть ли chapters в Symphonia `FormatReader`.
    has_chapters: bool,

    /// Есть ли текущая metadata revision в Symphonia metadata log.
    has_metadata_revision: bool,
}

/// Снимает format-level diagnostics до построения neutral track model.
fn summarize_symphonia_format_metadata(
    format: &mut FormatReaderBox<'static>,
) -> SymphoniaFormatMetadataSummary {
    let attachments = format.attachments().len();
    let has_chapters = format.chapters().is_some();
    let has_metadata_revision = format.metadata().current().is_some();

    SymphoniaFormatMetadataSummary {
        attachments,
        has_chapters,
        has_metadata_revision,
    }
}

/// Строит neutral track state из текущего Symphonia reader-а без изменения public boundary.
fn track_state_from_format_reader(
    format: &FormatReaderBox<'static>,
    video_tracks_by_track: &HashMap<TrackId, MatroskaVideoTrack>,
) -> SymphoniaTrackState {
    let mut video_tracks_for_mapping = video_tracks_by_track.clone();
    let track_mapping = map_tracks(format.tracks(), &mut video_tracks_for_mapping);
    let media_info_duration = media_info_duration(format.media_info());
    let duration = track_mapping.duration.or(media_info_duration);

    SymphoniaTrackState {
        tracks: track_mapping.tracks,
        duration,
        track_map: track_mapping.track_map,
        track_duration: track_mapping.duration,
        media_info_duration,
    }
}

/// Формирует компактный diagnostics snapshot для случая, когда reprobe меняет public identity.
fn track_layout_snapshot(tracks: &[TrackInfo], duration: Option<Duration>) -> String {
    let track_ids = tracks
        .iter()
        .map(|track| format!("{:?}:{:?}:{}", track.id, track.kind, track.codec_id))
        .collect::<Vec<_>>()
        .join(",");

    format!("duration={duration:?};tracks=[{track_ids}]")
}

impl Demuxer for SymphoniaDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        self.duration
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
    }

    fn next_packet(&mut self) -> Result<Option<OurPacket>> {
        loop {
            match self.next_event()? {
                DemuxReadEvent::Packet(packet) => return Ok(Some(packet)),
                DemuxReadEvent::EndOfStream => return Ok(None),
                DemuxReadEvent::TracksChanged(_) => continue,
            }
        }
    }

    fn next_event(&mut self) -> Result<DemuxReadEvent> {
        if let Some(event) = self.pending_events.pop_front() {
            return Ok(event);
        }

        self.read_next_event_from_format()
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let retained_lifecycle_events = self.take_pending_lifecycle_events();
        let was_at_end_of_stream = self.end_of_stream_reached;

        let seek_result = match request.mode {
            DemuxSeekMode::DecodePointBefore => {
                self.seek_decode_point_before(request, was_at_end_of_stream)
            }
            DemuxSeekMode::Accurate | DemuxSeekMode::Preview => {
                let seek_track_id = preferred_seek_track_id(&self.tracks);
                self.seek_symphonia_once(
                    request,
                    seek_track_id,
                    request.timestamp,
                    was_at_end_of_stream,
                )
            }
        };
        self.prepend_pending_events(retained_lifecycle_events);

        let seek_result = seek_result?;

        self.end_of_stream_reached = false;

        Ok(seek_result)
    }
}

impl SymphoniaDemuxer {
    /// Выполняет один backend seek и возвращает result относительно исходной пользовательской цели.
    fn seek_symphonia_once(
        &mut self,
        request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        backend_timestamp: Duration,
        reprobe_before_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let backend_request = DemuxSeekRequest {
            timestamp: backend_timestamp,
            mode: request.mode,
        };
        let seek_mode = symphonia_seek_mode(request.mode);
        let mut rebuilt_before_seek = false;
        if reprobe_before_seek && self.can_reprobe_current_source() {
            self.rebuild_format_reader_from_source_start()?;
            rebuilt_before_seek = true;
        }

        let seeked_to = match self.seek_symphonia_with_in_range_retry(
            seek_mode,
            backend_request,
            seek_track_id,
            "seek",
        ) {
            Ok(seeked_to) => seeked_to,
            Err(error)
                if reprobe_before_seek
                    && self.can_reprobe_current_source()
                    && !rebuilt_before_seek =>
            {
                warn!(
                    source = %self.source_label,
                    error = %error,
                    "Symphonia seek failed; rebuilding FormatReader and retrying once"
                );
                self.rebuild_format_reader_from_source_start()?;
                self.seek_symphonia_with_in_range_retry(
                    seek_mode,
                    backend_request,
                    seek_track_id,
                    "seek_after_reprobe",
                )?
            }
            Err(error) => return Err(error),
        };

        Ok(seeked_to_timeline_result(
            request.timestamp,
            seeked_to,
            &self.track_map,
        ))
    }

    /// Выполняет Symphonia seek и чинит in-range цели, которые backend считает концом stream-а.
    fn seek_symphonia_with_in_range_retry(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        operation: &'static str,
    ) -> Result<SeekedTo> {
        match self.seek_symphonia_raw(seek_mode, backend_request, seek_track_id, operation)? {
            Ok(seeked_to) => Ok(seeked_to),
            Err(error)
                if is_symphonia_out_of_range_seek_error(&error)
                    && self.can_reprobe_current_source()
                    && in_range_out_of_range_seek_can_retry(
                        backend_request.timestamp,
                        self.duration,
                    ) =>
            {
                self.retry_in_range_out_of_range_seek(
                    seek_mode,
                    backend_request,
                    seek_track_id,
                    error,
                )
            }
            Err(error) => Err(symphonia_seek_error_to_demux_error(error)),
        }
    }

    /// Один raw seek без адаптации ошибок; нужен, чтобы не потерять `SeekErrorKind`.
    fn seek_symphonia_raw(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        operation: &'static str,
    ) -> Result<std::result::Result<SeekedTo, SymphoniaError>> {
        let seek_target = symphonia_seek_target(backend_request, seek_track_id);

        Ok(self.format_mut(operation)?.seek(seek_mode, seek_target))
    }

    /// Для целей внутри public duration пробует ближайшие packet-safe позиции перед концом stream-а.
    fn retry_in_range_out_of_range_seek(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        original_error: SymphoniaError,
    ) -> Result<SeekedTo> {
        let mut failed_timestamp = backend_request.timestamp;
        let mut retry_offset = IN_RANGE_OUT_OF_RANGE_SEEK_INITIAL_RETRY_OFFSET;

        for retry_index in 0..IN_RANGE_OUT_OF_RANGE_SEEK_MAX_EXPONENTIAL_RETRIES {
            let retry_timestamp = backend_request.timestamp.saturating_sub(retry_offset);

            if retry_timestamp == failed_timestamp {
                break;
            }

            match self.attempt_in_range_out_of_range_retry(
                seek_mode,
                backend_request,
                seek_track_id,
                retry_timestamp,
                retry_index,
            )? {
                Ok(_) => {
                    return self.refine_in_range_out_of_range_seek(
                        seek_mode,
                        backend_request,
                        seek_track_id,
                        retry_timestamp,
                        failed_timestamp,
                    );
                }
                Err(error) if is_symphonia_out_of_range_seek_error(&error) => {
                    failed_timestamp = retry_timestamp;

                    if retry_timestamp.is_zero() {
                        break;
                    }

                    retry_offset = retry_offset
                        .checked_mul(2)
                        .unwrap_or(backend_request.timestamp);
                }
                Err(error) => return Err(symphonia_seek_error_to_demux_error(error)),
            }
        }

        Err(symphonia_seek_error_to_demux_error(original_error))
    }

    /// Уточняет найденный working timestamp вверх, чтобы не делать лишний audio/video pre-roll.
    fn refine_in_range_out_of_range_seek(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        mut accepted_timestamp: Duration,
        mut failed_timestamp: Duration,
    ) -> Result<SeekedTo> {
        for retry_index in 0..IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES {
            let search_window = failed_timestamp.saturating_sub(accepted_timestamp);
            if search_window <= IN_RANGE_OUT_OF_RANGE_SEEK_REFINEMENT_EPSILON {
                break;
            }

            let retry_timestamp = accepted_timestamp + search_window / 2;

            match self.attempt_in_range_out_of_range_retry(
                seek_mode,
                backend_request,
                seek_track_id,
                retry_timestamp,
                retry_index,
            )? {
                Ok(_) => {
                    accepted_timestamp = retry_timestamp;
                }
                Err(error) if is_symphonia_out_of_range_seek_error(&error) => {
                    failed_timestamp = retry_timestamp;
                }
                Err(error) => return Err(symphonia_seek_error_to_demux_error(error)),
            }
        }

        match self.attempt_in_range_out_of_range_retry(
            seek_mode,
            backend_request,
            seek_track_id,
            accepted_timestamp,
            IN_RANGE_OUT_OF_RANGE_SEEK_MAX_REFINEMENT_RETRIES,
        )? {
            Ok(seeked_to) => Ok(seeked_to),
            Err(error) => {
                warn!(
                    source = %self.source_label,
                    accepted_retry_ms = accepted_timestamp.as_millis(),
                    error = %error,
                    "Принятый fallback seek Symphonia не удалось повторить после уточнения"
                );
                Err(symphonia_seek_error_to_demux_error(error))
            }
        }
    }

    /// Делает одну retry-попытку из чистого reader-а, если source позволяет reprobe.
    fn attempt_in_range_out_of_range_retry(
        &mut self,
        seek_mode: SymphoniaSeekMode,
        backend_request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
        retry_timestamp: Duration,
        retry_index: usize,
    ) -> Result<std::result::Result<SeekedTo, SymphoniaError>> {
        if self.can_reprobe_current_source() {
            self.rebuild_format_reader_from_source_start()?;
        }

        debug!(
            source = %self.source_label,
            target_ms = backend_request.timestamp.as_millis(),
            retry_ms = retry_timestamp.as_millis(),
            retry_index,
            demux_mode = ?backend_request.mode,
            seek_track_id = ?seek_track_id,
            "Цель seek внутри public duration, но вне выбранного Symphonia stream; пробуем раньше"
        );

        let retry_request = DemuxSeekRequest {
            timestamp: retry_timestamp,
            mode: backend_request.mode,
        };

        self.seek_symphonia_raw(
            seek_mode,
            retry_request,
            seek_track_id,
            "seek_in_range_out_of_range_retry",
        )
    }

    /// Восстанавливает `DecodePointBefore`: успешный result не должен быть после requested target.
    fn seek_decode_point_before(
        &mut self,
        request: DemuxSeekRequest,
        reprobe_before_first_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let requested_timestamp = request.timestamp;
        let mut backend_timestamp = decode_point_before_initial_timestamp(
            requested_timestamp,
            self.options.decode_point_before_preroll(),
        );
        let mut retained_lifecycle_events = VecDeque::new();

        for retry_index in 0..=DECODE_POINT_BEFORE_MAX_RETRIES {
            let seek_track_id = preferred_seek_track_id(&self.tracks);
            let seek_result = self.seek_symphonia_once(
                request,
                seek_track_id,
                backend_timestamp,
                retry_index == 0 && reprobe_before_first_seek,
            )?;

            if let Some(video_track_id) = selected_video_track_id(&self.tracks) {
                let verification =
                    self.verify_decode_point_before_attempt(requested_timestamp, video_track_id)?;

                if let Some(issue) = verification.issue {
                    let Some(retry_timestamp) = decode_point_before_retry_timestamp_for_issue(
                        backend_timestamp,
                        requested_timestamp,
                        issue,
                        retry_index,
                        self.options.decode_point_before_preroll(),
                    ) else {
                        return Err(decode_point_before_verification_error(
                            requested_timestamp,
                            issue,
                            verification.packets_checked,
                            retry_index,
                        ));
                    };

                    if retry_index == DECODE_POINT_BEFORE_MAX_RETRIES
                        || retry_timestamp == backend_timestamp
                    {
                        return Err(decode_point_before_verification_error(
                            requested_timestamp,
                            issue,
                            verification.packets_checked,
                            retry_index,
                        ));
                    }

                    debug!(
                        target_ms = requested_timestamp.as_millis(),
                        retry_ms = retry_timestamp.as_millis(),
                        retry_index,
                        reason = issue.reason(),
                        packets_checked = verification.packets_checked,
                        first_video_pts_ms = issue.first_video_pts().map(|pts| pts.as_millis()),
                        first_video_keyframe = ?issue.first_video_keyframe(),
                        "Post-seek verification rejected DecodePointBefore; retrying earlier"
                    );

                    retain_tracks_changed_events_from_failed_verification(
                        &mut retained_lifecycle_events,
                        verification.buffered_events,
                    );
                    backend_timestamp = retry_timestamp;
                    continue;
                }

                if let Some(accepted_video_packet) = verification.accepted_video_packet {
                    log_decode_point_before_uncertainty(requested_timestamp, accepted_video_packet);
                    self.pending_events = prepend_retained_lifecycle_events(
                        retained_lifecycle_events,
                        verification.buffered_events,
                    );
                    return Ok(seek_result_with_verified_video_packet(
                        seek_result,
                        accepted_video_packet,
                    ));
                }

                self.pending_events = prepend_retained_lifecycle_events(
                    retained_lifecycle_events,
                    verification.buffered_events,
                );
                return Ok(seek_result);
            }

            let actual_timestamp = seek_result.actual_position.as_duration();
            if actual_timestamp <= requested_timestamp {
                return Ok(seek_result);
            }

            let Some(retry_timestamp) = decode_point_before_retry_timestamp(
                backend_timestamp,
                requested_timestamp,
                actual_timestamp,
                retry_index,
            ) else {
                return Err(decode_point_before_after_target_error(
                    requested_timestamp,
                    actual_timestamp,
                    retry_index,
                ));
            };

            if retry_index == DECODE_POINT_BEFORE_MAX_RETRIES
                || retry_timestamp == backend_timestamp
            {
                return Err(decode_point_before_after_target_error(
                    requested_timestamp,
                    actual_timestamp,
                    retry_index,
                ));
            }

            debug!(
                target_ms = requested_timestamp.as_millis(),
                actual_ms = actual_timestamp.as_millis(),
                retry_ms = retry_timestamp.as_millis(),
                retry_index,
                "Symphonia seek returned after target; retrying DecodePointBefore earlier"
            );

            backend_timestamp = retry_timestamp;
        }

        unreachable!("bounded DecodePointBefore retry loop always returns")
    }
}

/// Оставляет только lifecycle-события из rejected verification prefix-а.
fn retain_tracks_changed_events_from_failed_verification(
    retained_lifecycle_events: &mut VecDeque<DemuxReadEvent>,
    rejected_buffered_events: VecDeque<DemuxReadEvent>,
) {
    for event in rejected_buffered_events {
        if matches!(event, DemuxReadEvent::TracksChanged(_)) {
            retained_lifecycle_events.push_back(event);
        }
    }
}

/// Возвращает успешный verification prefix после lifecycle событий прошлых retry.
fn prepend_retained_lifecycle_events(
    mut retained_lifecycle_events: VecDeque<DemuxReadEvent>,
    successful_buffered_events: VecDeque<DemuxReadEvent>,
) -> VecDeque<DemuxReadEvent> {
    retained_lifecycle_events.extend(successful_buffered_events);
    retained_lifecycle_events
}

/// Packet-level наблюдение первого selected video packet-а после backend seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DecodePointBeforeVideoPacket {
    /// Нормализованный PTS, который увидит player pipeline.
    pts: Duration,

    /// Raw PTS selected video track-а, если container сообщил time base.
    track_pts: Option<TrackTimestamp>,

    /// Codec-aware keyframe-классификация packet-а.
    keyframe: PacketKeyframe,
}

impl DecodePointBeforeVideoPacket {
    /// Снимает только metadata, нужную seek verification-у, не забирая ownership packet-а.
    fn from_packet(packet: &OurPacket) -> Self {
        Self {
            pts: packet.pts,
            track_pts: packet.track_pts,
            keyframe: packet.keyframe,
        }
    }
}

/// Результат проверки одной backend seek-попытки на packet boundary.
struct DecodePointBeforeAttemptVerification {
    /// Events, которые verification прочитал и должен вернуть pipeline при успехе.
    buffered_events: VecDeque<DemuxReadEvent>,

    /// Сколько supported packets было проверено в этой попытке.
    packets_checked: usize,

    /// Первый packet выбранного video track-а, который можно использовать как decode-start.
    accepted_video_packet: Option<DecodePointBeforeVideoPacket>,

    /// Причина retry/error, если попытка не доказала decode-safe старт.
    issue: Option<DecodePointBeforeVerificationIssue>,
}

/// Почему packet-level проверка не приняла текущую backend seek-попытку.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodePointBeforeVerificationIssue {
    /// Первый selected video packet находится после пользовательской цели.
    FirstVideoAfterTarget {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },

    /// Packet до target найден, но он точно не является decode-start keyframe.
    FirstVideoNotKeyframe {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },

    /// В bounded prefix не встретился packet выбранного video track-а.
    NoVideoPacket {
        /// Сколько supported packets уже пришлось prebuffer-нуть.
        packets_checked: usize,
    },

    /// Первый selected video packet слишком далеко до пользовательской цели.
    FirstVideoTooFarBeforeTarget {
        /// Metadata первого selected video packet-а.
        packet: DecodePointBeforeVideoPacket,
    },
}

impl DecodePointBeforeVerificationIssue {
    /// Стабильная причина для diagnostics и typed demux error-а.
    const fn reason(self) -> &'static str {
        match self {
            Self::FirstVideoAfterTarget { .. } => "first_video_after_target",
            Self::FirstVideoNotKeyframe { .. } => "first_video_not_keyframe",
            Self::NoVideoPacket { .. } => "no_video_packet_in_verification_window",
            Self::FirstVideoTooFarBeforeTarget { .. } => "first_video_too_far_before_target",
        }
    }

    /// PTS первого selected video packet-а, если он был найден.
    const fn first_video_pts(self) -> Option<Duration> {
        match self {
            Self::FirstVideoAfterTarget { packet } | Self::FirstVideoNotKeyframe { packet } => {
                Some(packet.pts)
            }
            Self::FirstVideoTooFarBeforeTarget { packet } => Some(packet.pts),
            Self::NoVideoPacket { .. } => None,
        }
    }

    /// Keyframe-классификация первого selected video packet-а, если он был найден.
    const fn first_video_keyframe(self) -> Option<PacketKeyframe> {
        match self {
            Self::FirstVideoAfterTarget { packet } | Self::FirstVideoNotKeyframe { packet } => {
                Some(packet.keyframe)
            }
            Self::FirstVideoTooFarBeforeTarget { packet } => Some(packet.keyframe),
            Self::NoVideoPacket { .. } => None,
        }
    }
}

impl SymphoniaDemuxer {
    /// Проверяет, что после seek-а первый selected video packet является decode-start до target.
    fn verify_decode_point_before_attempt(
        &mut self,
        requested_timestamp: Duration,
        initial_video_track_id: TrackId,
    ) -> Result<DecodePointBeforeAttemptVerification> {
        let mut buffered_events = VecDeque::new();
        let mut packets_checked = 0_usize;
        let mut video_track_id = initial_video_track_id;
        let packet_limit = self.options.decode_point_before_verification_packet_limit();
        let max_accepted_preroll = self.options.decode_point_before_max_accepted_preroll();
        let mut unresolved_video_issue = None;

        while packets_checked < packet_limit {
            let event = self.read_next_event_from_format()?;

            match &event {
                DemuxReadEvent::Packet(packet) => {
                    packets_checked = packets_checked.saturating_add(1);

                    if packet.kind == TrackKind::Video && packet.track_id == video_track_id {
                        let video_packet = DecodePointBeforeVideoPacket::from_packet(packet);
                        let packet_issue = decode_point_before_packet_issue(
                            requested_timestamp,
                            video_packet,
                            max_accepted_preroll,
                        );

                        buffered_events.push_back(event);

                        match packet_issue {
                            None => {
                                return Ok(DecodePointBeforeAttemptVerification {
                                    buffered_events,
                                    packets_checked,
                                    accepted_video_packet: Some(video_packet),
                                    issue: None,
                                });
                            }
                            Some(DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
                                packet,
                            }) => {
                                return Ok(DecodePointBeforeAttemptVerification {
                                    buffered_events,
                                    packets_checked,
                                    accepted_video_packet: None,
                                    issue: Some(
                                        DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
                                            packet,
                                        },
                                    ),
                                });
                            }
                            Some(issue) => {
                                unresolved_video_issue =
                                    Some(decode_point_before_preferred_unresolved_issue(
                                        unresolved_video_issue,
                                        issue,
                                    ));
                                continue;
                            }
                        }
                    }
                }
                DemuxReadEvent::TracksChanged(_) => {
                    if let Some(updated_video_track_id) = selected_video_track_id(&self.tracks) {
                        video_track_id = updated_video_track_id;
                    }
                }
                DemuxReadEvent::EndOfStream => {
                    buffered_events.push_back(event);

                    return Ok(DecodePointBeforeAttemptVerification {
                        buffered_events,
                        packets_checked,
                        accepted_video_packet: None,
                        issue: Some(decode_point_before_unresolved_issue(
                            unresolved_video_issue,
                            packets_checked,
                        )),
                    });
                }
            }

            buffered_events.push_back(event);
        }

        Ok(DecodePointBeforeAttemptVerification {
            buffered_events,
            packets_checked,
            accepted_video_packet: None,
            issue: Some(decode_point_before_unresolved_issue(
                unresolved_video_issue,
                packets_checked,
            )),
        })
    }
}

/// Выбирает самую полезную причину, если bounded prefix ещё не дал decode-start.
fn decode_point_before_preferred_unresolved_issue(
    current: Option<DecodePointBeforeVerificationIssue>,
    incoming: DecodePointBeforeVerificationIssue,
) -> DecodePointBeforeVerificationIssue {
    match (current, incoming) {
        (
            Some(DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet }),
            DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { .. },
        ) => DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet },
        (_, issue) => issue,
    }
}

/// Завершает bounded verification typed причиной, не теряя уже найденный video context.
fn decode_point_before_unresolved_issue(
    unresolved_video_issue: Option<DecodePointBeforeVerificationIssue>,
    packets_checked: usize,
) -> DecodePointBeforeVerificationIssue {
    unresolved_video_issue
        .unwrap_or(DecodePointBeforeVerificationIssue::NoVideoPacket { packets_checked })
}

/// Выбирает первую backend-цель с pre-roll запасом, чтобы не стартовать после requested target.
fn decode_point_before_initial_timestamp(
    requested_timestamp: Duration,
    preroll: Duration,
) -> Duration {
    requested_timestamp.saturating_sub(preroll)
}

/// Считает следующую backend-цель по величине overshoot относительно исходного target-а.
fn decode_point_before_retry_timestamp(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
    actual_timestamp: Duration,
    retry_index: usize,
) -> Option<Duration> {
    let overshoot = actual_timestamp.checked_sub(requested_timestamp)?;
    let base_backoff = overshoot
        .checked_add(DECODE_POINT_BEFORE_RETRY_MARGIN)
        .unwrap_or(Duration::MAX);
    let retry_multiplier = 1_u32
        .checked_shl(retry_index.min(31) as u32)
        .unwrap_or(u32::MAX);
    let retry_backoff = base_backoff
        .checked_mul(retry_multiplier)
        .unwrap_or(Duration::MAX);

    Some(backend_timestamp.saturating_sub(retry_backoff))
}

/// Выбирает video track, для которого `DecodePointBefore` должен доказать packet-level старт.
fn selected_video_track_id(tracks: &[TrackInfo]) -> Option<TrackId> {
    tracks
        .iter()
        .find(|track| track.kind == TrackKind::Video)
        .map(|track| track.id)
}

/// Отличает конец конкретного Symphonia stream-а от других seek failures.
fn is_symphonia_out_of_range_seek_error(error: &SymphoniaError) -> bool {
    matches!(error, SymphoniaError::SeekError(SeekErrorKind::OutOfRange))
}

/// Retry допустим только для цели, которую public timeline уже объявил достижимой.
fn in_range_out_of_range_seek_can_retry(
    backend_timestamp: Duration,
    duration: Option<Duration>,
) -> bool {
    duration.is_some_and(|duration| !backend_timestamp.is_zero() && backend_timestamp <= duration)
}

/// Классифицирует первый selected video packet относительно `DecodePointBefore` contract-а.
fn decode_point_before_packet_issue(
    requested_timestamp: Duration,
    packet: DecodePointBeforeVideoPacket,
    max_accepted_preroll: Duration,
) -> Option<DecodePointBeforeVerificationIssue> {
    if packet.pts > requested_timestamp {
        return Some(DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet });
    }

    if requested_timestamp.saturating_sub(packet.pts) > max_accepted_preroll {
        return Some(DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { packet });
    }

    if packet.keyframe == PacketKeyframe::NotKeyframe {
        return Some(DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { packet });
    }

    None
}

/// Считает retry target для packet-level failure без смешивания разных причин.
fn decode_point_before_retry_timestamp_for_issue(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
    issue: DecodePointBeforeVerificationIssue,
    retry_index: usize,
    preroll: Duration,
) -> Option<Duration> {
    match issue {
        DecodePointBeforeVerificationIssue::FirstVideoAfterTarget { packet } => {
            if backend_timestamp < requested_timestamp {
                // Если backend уже искал раньше requested, но первый video packet всё равно
                // оказался после target, маленький отступ на величину packet overshoot-а
                // обычно остаётся внутри того же cue/cluster. Расширяем pre-roll окно, чтобы
                // действительно перейти к предыдущей decode-точке.
                Some(decode_point_before_expanding_retry_timestamp(
                    backend_timestamp,
                    retry_index,
                    preroll,
                ))
            } else {
                decode_point_before_retry_timestamp(
                    backend_timestamp,
                    requested_timestamp,
                    packet.pts,
                    retry_index,
                )
            }
        }
        DecodePointBeforeVerificationIssue::FirstVideoTooFarBeforeTarget { .. } => {
            decode_point_before_rescue_retry_timestamp(backend_timestamp, requested_timestamp)
        }
        DecodePointBeforeVerificationIssue::FirstVideoNotKeyframe { .. }
        | DecodePointBeforeVerificationIssue::NoVideoPacket { .. } => Some(
            decode_point_before_expanding_retry_timestamp(backend_timestamp, retry_index, preroll),
        ),
    }
}

/// Пробует rescue seek ближе к target, если backend прыгнул слишком далеко назад.
fn decode_point_before_rescue_retry_timestamp(
    backend_timestamp: Duration,
    requested_timestamp: Duration,
) -> Option<Duration> {
    (backend_timestamp < requested_timestamp).then_some(requested_timestamp)
}

/// Отодвигает backend target назад, когда packet prefix не дал usable video decode-start.
fn decode_point_before_expanding_retry_timestamp(
    backend_timestamp: Duration,
    retry_index: usize,
    preroll: Duration,
) -> Duration {
    let base_backoff = if preroll.is_zero() {
        DECODE_POINT_BEFORE_RETRY_MARGIN
    } else {
        preroll
    };
    let retry_multiplier = 1_u32
        .checked_shl(retry_index.min(31) as u32)
        .unwrap_or(u32::MAX);
    let retry_backoff = base_backoff
        .checked_mul(retry_multiplier)
        .unwrap_or(Duration::MAX);

    backend_timestamp.saturating_sub(retry_backoff)
}

/// Подменяет backend `SeekedTo.actual_ts` packet-level video timestamp-ом успешной проверки.
fn seek_result_with_verified_video_packet(
    mut seek_result: DemuxSeekResult,
    first_video_packet: DecodePointBeforeVideoPacket,
) -> DemuxSeekResult {
    seek_result.actual_position = MediaTime::from_duration(first_video_packet.pts);
    seek_result.actual_track_timestamp = first_video_packet.track_pts;
    seek_result
}

/// Логирует случаи, где PTS contract доказан, а keyframe-классификация осталась неопределённой.
fn log_decode_point_before_uncertainty(
    requested_timestamp: Duration,
    first_video_packet: DecodePointBeforeVideoPacket,
) {
    if first_video_packet.keyframe != PacketKeyframe::Unknown {
        return;
    }

    warn!(
        target_ms = requested_timestamp.as_millis(),
        first_video_pts_ms = first_video_packet.pts.as_millis(),
        first_video_track_timestamp = ?first_video_packet.track_pts,
        "DecodePointBefore accepted first video packet with unknown keyframe status"
    );
}

/// Создаёт typed ошибку packet-level проверки `DecodePointBefore`.
fn decode_point_before_verification_error(
    requested_timestamp: Duration,
    issue: DecodePointBeforeVerificationIssue,
    packets_checked: usize,
    retry_index: usize,
) -> anyhow::Error {
    let effective_packets_checked = match issue {
        DecodePointBeforeVerificationIssue::NoVideoPacket { packets_checked } => packets_checked,
        _ => packets_checked,
    };

    DemuxError::DecodePointBeforeVerificationFailed {
        reason: issue.reason(),
        requested_position: requested_timestamp,
        attempts: retry_index + 1,
        packets_checked: effective_packets_checked,
        first_video_pts: issue.first_video_pts(),
        first_video_keyframe: issue.first_video_keyframe(),
    }
    .into()
}

/// Создаёт ошибку, если backend не смог честно выполнить before-or-at-target seek.
fn decode_point_before_after_target_error(
    requested_timestamp: Duration,
    _actual_timestamp: Duration,
    retry_index: usize,
) -> anyhow::Error {
    DemuxError::DecodePointBeforeVerificationFailed {
        reason: "backend_actual_after_target",
        requested_position: requested_timestamp,
        attempts: retry_index + 1,
        packets_checked: 0,
        first_video_pts: None,
        first_video_keyframe: None,
    }
    .into()
}

/// Конвертирует source seekability в neutral demux seekability.
fn source_seekability_to_demux_seekability(seekability: SourceSeekability) -> DemuxSeekability {
    match seekability {
        SourceSeekability::Seekable => DemuxSeekability::Seekable,
        SourceSeekability::NotSeekable { reason } => match reason {
            source_core::NotSeekableReason::HttpRangeStatus { .. } => {
                DemuxSeekability::NotSeekable {
                    reason: TimelineNotSeekableReason::SourceNotSeekable,
                }
            }
            source_core::NotSeekableReason::Unknown => DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::UnknownTimeline,
            },
        },
    }
}

/// Запускает Matroska pre-scan только для Matroska/WebM video/unknown кандидатов.
fn extract_video_tracks_from_file_if_needed(
    path: &Path,
    tracks: &[symphonia::core::formats::Track],
) -> HashMap<TrackId, MatroskaVideoTrack> {
    let extension_hint = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    let decision = decide_matroska_video_metadata_scan(extension_hint, tracks);
    if decision != MatroskaVideoMetadataScanDecision::Scan {
        trace!(
            path = %path.display(),
            reason = decision.reason(),
            "Matroska video metadata pre-scan skipped for file"
        );
        return HashMap::new();
    }

    match extract_video_tracks_from_file(path) {
        Ok(video_tracks_by_track) => video_tracks_by_track,
        Err(error) => {
            warn!(
                error = %error,
                path = %path.display(),
                "Matroska video track pre-scan failed"
            );
            HashMap::new()
        }
    }
}

/// Решение о запуске Matroska/WebM scan после того, как Symphonia уже отдала track list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MatroskaVideoMetadataScanDecision {
    /// Нужно читать bounded Matroska prefix для video/HDR fallback-а.
    Scan,

    /// Расширение не относится к Matroska/WebM, scan был бы контейнерным костылём.
    SkipNonMatroskaContainer,

    /// Symphonia не показала video/unknown кандидатов, значит video fallback не нужен.
    SkipNoVideoCandidates,
}

impl MatroskaVideoMetadataScanDecision {
    /// Стабильная причина для diagnostics.
    const fn reason(self) -> &'static str {
        match self {
            Self::Scan => "scan",
            Self::SkipNonMatroskaContainer => "non_matroska_container",
            Self::SkipNoVideoCandidates => "no_video_or_unknown_tracks",
        }
    }
}

/// Решает, нужен ли Matroska fallback для уже распробованного Symphonia reader-а.
fn decide_matroska_video_metadata_scan(
    extension_hint: &str,
    tracks: &[symphonia::core::formats::Track],
) -> MatroskaVideoMetadataScanDecision {
    if !extension_may_have_matroska_video_metadata(extension_hint) {
        return MatroskaVideoMetadataScanDecision::SkipNonMatroskaContainer;
    }

    if !tracks_may_need_matroska_video_metadata(tracks) {
        return MatroskaVideoMetadataScanDecision::SkipNoVideoCandidates;
    }

    MatroskaVideoMetadataScanDecision::Scan
}

/// Возвращает `true` для контейнеров, где Matroska prefix scan может дать video metadata.
fn extension_may_have_matroska_video_metadata(extension_hint: &str) -> bool {
    matches!(
        extension_hint
            .trim()
            .trim_start_matches('.')
            .to_ascii_lowercase()
            .as_str(),
        "mkv" | "webm"
    )
}

/// Читает Matroska prefix из seekable byte source-а и возвращает source cursor назад.
fn extract_video_tracks_from_byte_source<S>(
    source: &mut S,
    label: &str,
) -> Result<HashMap<TrackId, MatroskaVideoTrack>, DemuxError>
where
    S: ByteSource,
{
    if !source.seekability().is_seekable() {
        debug!(
            source = %label,
            "Matroska video metadata byte-source pre-scan skipped for unseekable source"
        );
        return Ok(HashMap::new());
    }

    let original_position = source.position();
    let scan_result = read_byte_source_video_tracks(source);
    let reset_result = source.seek(original_position);

    if let Err(error) = reset_result {
        return Err(source_error_to_demux_error(error));
    }

    match scan_result {
        Ok(video_tracks_by_track) => Ok(video_tracks_by_track),
        Err(error) => {
            warn!(
                error = %error,
                source = %label,
                "Matroska video track byte-source pre-scan failed"
            );
            Ok(HashMap::new())
        }
    }
}

/// Читает короткий prefix unseekable stream-а и потом replay-ит его перед основным reader-ом.
fn read_stream_prefix<R>(
    reader: &mut R,
) -> io::Result<(Vec<u8>, HashMap<TrackId, MatroskaVideoTrack>)>
where
    R: Read,
{
    let mut metadata_prefix = Vec::new();
    let mut read_buffer = [0_u8; 64 * 1024];

    while metadata_prefix.len() < MATROSKA_STREAM_SCAN_LIMIT_BYTES {
        let remaining_bytes = MATROSKA_STREAM_SCAN_LIMIT_BYTES - metadata_prefix.len();
        let read_size = remaining_bytes.min(read_buffer.len());
        let bytes_read = reader.read(&mut read_buffer[..read_size])?;

        if bytes_read == 0 {
            break;
        }

        metadata_prefix.extend_from_slice(&read_buffer[..bytes_read]);

        let scan = scan_video_tracks_from_bytes(&metadata_prefix);
        if scan.tracks_found {
            return Ok((metadata_prefix, scan.video_tracks));
        }
    }

    let scan = scan_video_tracks_from_bytes(&metadata_prefix);
    Ok((metadata_prefix, scan.video_tracks))
}

/// Читает prefix seekable byte source-а только до первого найденного Matroska `Tracks`.
fn read_byte_source_video_tracks<S>(
    source: &mut S,
) -> SourceResult<HashMap<TrackId, MatroskaVideoTrack>>
where
    S: ByteSource,
{
    let cancellation = CancellationToken::never_cancelled();
    let mut metadata_prefix = Vec::new();
    let mut read_buffer = [0_u8; 64 * 1024];

    while metadata_prefix.len() < MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES {
        let remaining_bytes = MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES - metadata_prefix.len();
        let read_size = remaining_bytes.min(read_buffer.len());
        let bytes_read = source.read(&mut read_buffer[..read_size], &cancellation)?;

        if bytes_read == 0 {
            break;
        }

        metadata_prefix.extend_from_slice(&read_buffer[..bytes_read]);

        let scan = scan_video_tracks_from_bytes(&metadata_prefix);
        if scan.tracks_found {
            return Ok(scan.video_tracks);
        }
    }

    Ok(scan_video_tracks_from_bytes(&metadata_prefix).video_tracks)
}

/// Конвертирует source-layer ошибку pre-scan-а в demux-level IO ошибку.
fn source_error_to_demux_error(error: SourceError) -> DemuxError {
    DemuxError::Io(io::Error::other(error))
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fs::File;
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use media_core::{
        DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, DemuxTrackListUpdate, Demuxer,
        MediaDemuxError, PacketKeyframe, TimelineNotSeekableReason, TrackId, TrackKind,
    };
    use symphonia::core::audio::{Channels, Position};
    use symphonia::core::codecs::CodecParameters;
    use symphonia::core::codecs::audio::AudioCodecParameters;
    use symphonia::core::codecs::audio::well_known as audio_codec;
    use symphonia::core::codecs::subtitle::SubtitleCodecParameters;
    use symphonia::core::codecs::subtitle::well_known as subtitle_codec;
    use symphonia::core::codecs::video::VideoCodecParameters;
    use symphonia::core::codecs::video::well_known as video_codec;
    use symphonia::core::errors::Error as SymphoniaError;
    use symphonia::core::formats::{
        FORMAT_ID_NULL, FormatInfo, FormatReader, MediaInfo, SeekMode, SeekTo, SeekedTo, Track,
    };
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::{Metadata, MetadataLog};
    use symphonia::core::packet::Packet;
    use symphonia::core::units::{Duration as SymphoniaDuration, TimeBase, Timestamp};

    use super::{
        DECODE_POINT_BEFORE_MAX_RETRIES, DecodePointBeforeVerificationIssue,
        DecodePointBeforeVideoPacket, MATROSKA_STREAM_SCAN_LIMIT_BYTES,
        MatroskaVideoMetadataScanDecision, SymphoniaDemuxer, decide_matroska_video_metadata_scan,
        decode_point_before_retry_timestamp_for_issue, read_stream_prefix,
    };
    use crate::error::DemuxError;
    use crate::matroska_metadata::MatroskaVideoTrack;
    use crate::options::DemuxerOptions;

    const MAX_UNIT_EVENTS_BEFORE_EOF: usize = 16_384;
    const MAX_UNIT_EVENTS_AFTER_SEEK: usize = 512;

    struct FakeFormatReader {
        format_info: FormatInfo,
        media_info: MediaInfo,
        tracks: Vec<Track>,
        reset_track_updates: VecDeque<Vec<Track>>,
        metadata: MetadataLog,
        packets: VecDeque<std::result::Result<Packet, SymphoniaError>>,
        seek_packet_scripts: VecDeque<VecDeque<std::result::Result<Packet, SymphoniaError>>>,
        seek_mode_log: Option<Arc<Mutex<Vec<SeekMode>>>>,
        seek_track_log: Option<Arc<Mutex<Vec<u32>>>>,
        next_packet_call_count: Option<Arc<Mutex<usize>>>,
        seek_response_policy: FakeSeekResponsePolicy,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeSeekResponsePolicy {
        Zero,
        CoarseAfterTargetAccurateBefore,
    }

    impl FakeFormatReader {
        fn new(
            tracks: Vec<Track>,
            packets: Vec<std::result::Result<Packet, SymphoniaError>>,
        ) -> Self {
            Self {
                format_info: FormatInfo {
                    format: FORMAT_ID_NULL,
                    short_name: "fake",
                    long_name: "Fake FormatReader",
                },
                media_info: MediaInfo::default(),
                tracks,
                reset_track_updates: VecDeque::new(),
                metadata: MetadataLog::default(),
                packets: VecDeque::from(packets),
                seek_packet_scripts: VecDeque::new(),
                seek_mode_log: None,
                seek_track_log: None,
                next_packet_call_count: None,
                seek_response_policy: FakeSeekResponsePolicy::Zero,
            }
        }

        fn with_seek_mode_log(mut self, seek_mode_log: Arc<Mutex<Vec<SeekMode>>>) -> Self {
            self.seek_mode_log = Some(seek_mode_log);
            self
        }

        fn with_seek_track_log(mut self, seek_track_log: Arc<Mutex<Vec<u32>>>) -> Self {
            self.seek_track_log = Some(seek_track_log);
            self
        }

        fn with_next_packet_call_count(mut self, call_count: Arc<Mutex<usize>>) -> Self {
            self.next_packet_call_count = Some(call_count);
            self
        }

        fn with_seek_response_policy(mut self, policy: FakeSeekResponsePolicy) -> Self {
            self.seek_response_policy = policy;
            self
        }

        fn with_seek_packet_scripts(
            mut self,
            scripts: Vec<Vec<std::result::Result<Packet, SymphoniaError>>>,
        ) -> Self {
            self.seek_packet_scripts = scripts
                .into_iter()
                .map(VecDeque::from)
                .collect::<VecDeque<_>>();
            self
        }

        fn with_media_info(mut self, media_info: MediaInfo) -> Self {
            self.media_info = media_info;
            self
        }

        fn with_reset_track_update(mut self, tracks: Vec<Track>) -> Self {
            self.reset_track_updates.push_back(tracks);
            self
        }

        fn seek_response(&self, mode: SeekMode, target: SeekTo) -> SeekedTo {
            let track_id = seek_target_track_id(&self.tracks, &target);
            let required_ts = required_seek_timestamp(&self.tracks, &target);
            let actual_ts = match self.seek_response_policy {
                FakeSeekResponsePolicy::Zero => Timestamp::ZERO,
                FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore => match mode {
                    SeekMode::Coarse => required_ts.saturating_add(SymphoniaDuration::new(250)),
                    SeekMode::Accurate => required_ts.saturating_sub(SymphoniaDuration::new(250)),
                },
            };

            SeekedTo {
                track_id,
                required_ts,
                actual_ts,
            }
        }
    }

    impl FormatReader for FakeFormatReader {
        fn format_info(&self) -> &FormatInfo {
            &self.format_info
        }

        fn media_info(&self) -> &MediaInfo {
            &self.media_info
        }

        fn metadata(&mut self) -> Metadata<'_> {
            self.metadata.metadata()
        }

        fn seek(
            &mut self,
            mode: SeekMode,
            target: SeekTo,
        ) -> symphonia::core::errors::Result<SeekedTo> {
            if let Some(ref seek_mode_log) = self.seek_mode_log {
                seek_mode_log
                    .lock()
                    .expect("seek mode log mutex should not be poisoned")
                    .push(mode);
            }
            if let Some(ref seek_track_log) = self.seek_track_log {
                seek_track_log
                    .lock()
                    .expect("seek track log mutex should not be poisoned")
                    .push(seek_target_track_id(&self.tracks, &target));
            }
            if let Some(packets) = self.seek_packet_scripts.pop_front() {
                self.packets = packets;
            }

            Ok(self.seek_response(mode, target))
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
            if let Some(ref call_count) = self.next_packet_call_count {
                let mut call_count = call_count
                    .lock()
                    .expect("next_packet call count mutex should not be poisoned");
                *call_count += 1;
            }

            match self.packets.pop_front() {
                Some(Ok(packet)) => Ok(Some(packet)),
                Some(Err(SymphoniaError::ResetRequired)) => {
                    if let Some(next_tracks) = self.reset_track_updates.pop_front() {
                        self.tracks = next_tracks;
                    }
                    Err(SymphoniaError::ResetRequired)
                }
                Some(Err(error)) => Err(error),
                None => Ok(None),
            }
        }

        fn into_inner<'source>(self: Box<Self>) -> MediaSourceStream<'source>
        where
            Self: 'source,
        {
            unreachable!("tests не возвращают MediaSourceStream из FakeFormatReader");
        }
    }

    fn seek_target_track_id(tracks: &[Track], target: &SeekTo) -> u32 {
        match target {
            SeekTo::Time { track_id, .. } => track_id
                .and_then(|id| tracks.iter().find(|track| track.id == id))
                .or_else(|| tracks.first())
                .map(|track| track.id)
                .unwrap_or_default(),
            SeekTo::Timestamp { track_id, .. } => *track_id,
        }
    }

    fn required_seek_timestamp(tracks: &[Track], target: &SeekTo) -> Timestamp {
        match target {
            SeekTo::Time { time, track_id } => track_id
                .and_then(|id| tracks.iter().find(|track| track.id == id))
                .or_else(|| tracks.first())
                .and_then(|track| track.time_base)
                .and_then(|time_base| time_base.calc_timestamp(*time))
                .unwrap_or(Timestamp::ZERO),
            SeekTo::Timestamp { ts, .. } => *ts,
        }
    }

    fn test_webm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/test.webm")
    }

    fn audio_fixture_path(file_name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../test-assets/audio")
            .join(file_name)
    }

    fn drain_demuxer_to_eof_for_unit_test(demuxer: &mut SymphoniaDemuxer) {
        for event_index in 0..MAX_UNIT_EVENTS_BEFORE_EOF {
            match demuxer
                .next_event()
                .unwrap_or_else(|error| panic!("EOF drain event #{event_index} failed: {error}"))
            {
                DemuxReadEvent::EndOfStream => return,
                DemuxReadEvent::Packet(_) | DemuxReadEvent::TracksChanged(_) => {}
            }
        }

        panic!("demuxer did not reach EOF within {MAX_UNIT_EVENTS_BEFORE_EOF} events in unit test");
    }

    fn assert_first_packet_after_seek_for_unit_test(demuxer: &mut SymphoniaDemuxer) {
        for event_index in 0..MAX_UNIT_EVENTS_AFTER_SEEK {
            match demuxer.next_event().unwrap_or_else(|error| {
                panic!("post-seek event #{event_index} failed in unit test: {error}")
            }) {
                DemuxReadEvent::Packet(_) => return,
                DemuxReadEvent::TracksChanged(_) => {}
                DemuxReadEvent::EndOfStream => {
                    panic!("post-seek EOF arrived before any packet in unit test")
                }
            }
        }

        panic!(
            "demuxer did not return a packet within {MAX_UNIT_EVENTS_AFTER_SEEK} post-seek events"
        );
    }

    fn vp9_video_track(track_id: u32) -> Track {
        let mut video_params = VideoCodecParameters::default();
        video_params.for_codec(video_codec::CODEC_ID_VP9);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Video(video_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track
    }

    fn aac_audio_track_with_timing(track_id: u32, duration: SymphoniaDuration) -> Track {
        let mut audio_params = AudioCodecParameters::new();
        audio_params.for_codec(audio_codec::CODEC_ID_AAC);
        audio_params.with_sample_rate(48_000);
        audio_params.with_channels(Channels::from(Position::FRONT_LEFT | Position::FRONT_RIGHT));

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Audio(audio_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track.with_duration(duration);
        track
    }

    fn media_info_with_duration(duration: SymphoniaDuration) -> MediaInfo {
        let mut media_info = MediaInfo::default();
        media_info.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        media_info.with_duration(duration);
        media_info
    }

    fn unknown_track(track_id: u32) -> Track {
        let mut track = Track::new(track_id);
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track
    }

    fn subtitle_track(track_id: u32) -> Track {
        let mut subtitle_params = SubtitleCodecParameters::new();
        subtitle_params.for_codec(subtitle_codec::CODEC_ID_WEBVTT);

        let mut track = Track::new(track_id);
        track.with_codec_params(CodecParameters::Subtitle(subtitle_params));
        track.with_time_base(TimeBase::try_new(1, 1_000).expect("valid time base"));
        track
    }

    fn fake_packet(track_id: u32, timestamp: i64, packet_bytes: Vec<u8>) -> Packet {
        Packet::new(
            track_id,
            Timestamp::new(timestamp),
            SymphoniaDuration::new(1),
            packet_bytes,
        )
    }

    fn small_vp9_keyframe_packet(track_id: u32, timestamp: i64) -> Packet {
        fake_packet(track_id, timestamp, build_vp9_keyframe())
    }

    fn small_vp9_inter_frame_packet(track_id: u32, timestamp: i64) -> Packet {
        fake_packet(track_id, timestamp, build_vp9_inter_frame())
    }

    fn build_vp9_keyframe() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(0);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0x498342, 24);
        push_bits(&mut bits, 1, 3);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn build_vp9_inter_frame() -> Vec<u8> {
        let mut bits = Vec::new();
        push_bits(&mut bits, 0b10, 2);
        push_profile(&mut bits, 0);
        bits.push(0);
        bits.push(1);
        bits.push(1);
        bits.push(0);
        push_bits(&mut bits, 0, 2);
        push_bits(&mut bits, 0x01, 8);
        push_bits(&mut bits, 1, 3);
        bits.push(1);
        push_bits(&mut bits, 2, 3);
        bits.push(0);
        push_bits(&mut bits, 3, 3);
        bits.push(1);
        bits.push(0);
        bits.push(0);
        bits.push(0);
        push_bits(&mut bits, 63, 16);
        push_bits(&mut bits, 63, 16);
        bits.push(0);
        bits_to_bytes(&bits)
    }

    fn bits_to_bytes(bits: &[u8]) -> Vec<u8> {
        bits.chunks(8)
            .map(|chunk| {
                let mut byte = 0_u8;
                for (index, bit) in chunk.iter().enumerate() {
                    byte |= bit << (7 - index);
                }
                byte
            })
            .collect()
    }

    fn push_bits(bits: &mut Vec<u8>, value: u32, width: u8) {
        for shift in (0..width).rev() {
            bits.push(((value >> shift) & 1) as u8);
        }
    }

    fn push_profile(bits: &mut Vec<u8>, profile: u8) {
        bits.push(profile & 1);
        bits.push((profile >> 1) & 1);
        if profile == 3 {
            bits.push(0);
        }
    }

    fn fake_demuxer_with_options(
        packets: Vec<std::result::Result<Packet, SymphoniaError>>,
        matroska_tracks: HashMap<TrackId, MatroskaVideoTrack>,
        options: DemuxerOptions,
    ) -> SymphoniaDemuxer {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], packets);
        SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            matroska_tracks,
            DemuxSeekability::Seekable,
            options,
        )
        .expect("fake demuxer должен открыться")
    }

    fn fake_demuxer_with_seek_mode_log() -> (SymphoniaDemuxer, Arc<Mutex<Vec<SeekMode>>>) {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 0))]])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        (demuxer, seek_mode_log)
    }

    fn assert_symphonia_seek_mode(request: DemuxSeekRequest, expected_mode: SeekMode) {
        let (mut demuxer, seek_mode_log) = fake_demuxer_with_seek_mode_log();

        demuxer
            .seek_with_request(request)
            .expect("fake seek должен завершиться без ошибки");

        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .as_slice(),
            &[expected_mode]
        );
    }

    fn next_video_packet(demuxer: &mut SymphoniaDemuxer) -> media_core::Packet {
        loop {
            let packet = demuxer
                .next_packet()
                .expect("packet read должен завершиться без ошибки")
                .expect("test asset должен содержать video packet");

            if packet.kind == TrackKind::Video {
                return packet;
            }
        }
    }

    struct BoundedPrefixReader {
        bytes_remaining: usize,
        bytes_read: usize,
    }

    impl BoundedPrefixReader {
        fn new(bytes_remaining: usize) -> Self {
            Self {
                bytes_remaining,
                bytes_read: 0,
            }
        }
    }

    impl std::io::Read for BoundedPrefixReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            let bytes_to_read = self.bytes_remaining.min(output.len());
            if bytes_to_read == 0 {
                return Ok(0);
            }

            output[..bytes_to_read].fill(0);
            self.bytes_remaining -= bytes_to_read;
            self.bytes_read += bytes_to_read;
            Ok(bytes_to_read)
        }
    }

    #[test]
    fn non_matroska_audio_extensions_skip_matroska_video_scan() {
        let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

        for extension in ["ogg", "mp3", "wav"] {
            assert_eq!(
                decide_matroska_video_metadata_scan(extension, std::slice::from_ref(&audio_track)),
                MatroskaVideoMetadataScanDecision::SkipNonMatroskaContainer
            );
        }
    }

    #[test]
    fn audio_only_webm_skips_matroska_video_scan_after_symphonia_probe() {
        let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

        assert_eq!(
            decide_matroska_video_metadata_scan("webm", &[audio_track]),
            MatroskaVideoMetadataScanDecision::SkipNoVideoCandidates
        );
    }

    #[test]
    fn stream_prefix_scan_limit_stays_bounded() {
        assert_eq!(MATROSKA_STREAM_SCAN_LIMIT_BYTES, 256 * 1024);

        let mut reader = BoundedPrefixReader::new(MATROSKA_STREAM_SCAN_LIMIT_BYTES * 2);
        let (prefix, video_tracks_by_track) =
            read_stream_prefix(&mut reader).expect("bounded prefix read works");

        assert_eq!(prefix.len(), MATROSKA_STREAM_SCAN_LIMIT_BYTES);
        assert_eq!(reader.bytes_read, MATROSKA_STREAM_SCAN_LIMIT_BYTES);
        assert!(video_tracks_by_track.is_empty());
    }

    #[test]
    fn audio_only_demuxer_opens_and_uses_track_duration_metadata() {
        let reader = FakeFormatReader::new(
            vec![aac_audio_track_with_timing(
                2,
                SymphoniaDuration::new(30_000),
            )],
            Vec::new(),
        );

        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "audio-only",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("audio-only demuxer должен открыться без video track-а");

        assert_eq!(demuxer.tracks().len(), 1);
        assert_eq!(demuxer.tracks()[0].kind, TrackKind::Audio);
        assert_eq!(demuxer.tracks()[0].codec_id, "A_AAC");
        assert_eq!(demuxer.tracks()[0].sample_rate, Some(48_000));
        assert_eq!(demuxer.tracks()[0].channels, Some(2));
        assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    }

    #[test]
    fn demuxer_uses_media_info_duration_when_tracks_do_not_have_duration() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "media-info-duration",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться с container-level duration");

        assert_eq!(demuxer.tracks().len(), 1);
        assert_eq!(demuxer.tracks()[0].kind, TrackKind::Video);
        assert_eq!(demuxer.tracks()[0].duration, None);
        assert_eq!(demuxer.duration(), Some(Duration::from_secs(12)));
        assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
    }

    #[test]
    fn demuxer_prefers_track_duration_over_media_info_duration() {
        let reader = FakeFormatReader::new(
            vec![aac_audio_track_with_timing(
                2,
                SymphoniaDuration::new(30_000),
            )],
            Vec::new(),
        )
        .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "track-duration-wins",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен сохранить track-level duration");

        assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
        assert_eq!(demuxer.tracks()[0].duration, Some(Duration::from_secs(30)));
    }

    #[test]
    fn reset_required_refreshes_track_list_as_lifecycle_event() {
        let reader = FakeFormatReader::new(
            vec![aac_audio_track_with_timing(
                2,
                SymphoniaDuration::new(30_000),
            )],
            vec![Err(SymphoniaError::ResetRequired)],
        )
        .with_reset_track_update(vec![aac_audio_track_with_timing(
            3,
            SymphoniaDuration::new(42_000),
        )]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "reset-event",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let event = demuxer
            .next_event()
            .expect("ResetRequired должен стать lifecycle event");

        match event {
            DemuxReadEvent::TracksChanged(track_update) => {
                assert_eq!(track_update.tracks.len(), 1);
                assert_eq!(track_update.tracks[0].id, TrackId::new(3));
                assert_eq!(track_update.duration, Some(Duration::from_secs(42)));
            }
            unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
        }
        assert_eq!(demuxer.tracks()[0].id, TrackId::new(3));
        assert_eq!(demuxer.duration(), Some(Duration::from_secs(42)));
    }

    #[test]
    fn reset_required_keeps_media_info_duration_when_tracks_do_not_have_duration() {
        let reader = FakeFormatReader::new(
            vec![vp9_video_track(1)],
            vec![Err(SymphoniaError::ResetRequired)],
        )
        .with_media_info(media_info_with_duration(SymphoniaDuration::new(18_000)))
        .with_reset_track_update(vec![vp9_video_track(4)]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "reset-media-info-duration",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться с container-level duration");

        let event = demuxer
            .next_event()
            .expect("ResetRequired должен обновить track list без потери media duration");

        match event {
            DemuxReadEvent::TracksChanged(track_update) => {
                assert_eq!(track_update.tracks.len(), 1);
                assert_eq!(track_update.tracks[0].id, TrackId::new(4));
                assert_eq!(track_update.duration, Some(Duration::from_secs(18)));
            }
            unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
        }
        assert_eq!(demuxer.tracks()[0].id, TrackId::new(4));
        assert_eq!(demuxer.tracks()[0].duration, None);
        assert_eq!(demuxer.duration(), Some(Duration::from_secs(18)));
    }

    #[test]
    fn next_packet_compatibility_skips_reset_lifecycle_event() {
        let reader = FakeFormatReader::new(
            vec![aac_audio_track_with_timing(
                2,
                SymphoniaDuration::new(30_000),
            )],
            vec![
                Err(SymphoniaError::ResetRequired),
                Ok(fake_packet(3, 5, b"audio".to_vec())),
            ],
        )
        .with_reset_track_update(vec![aac_audio_track_with_timing(
            3,
            SymphoniaDuration::new(42_000),
        )]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "reset-next-packet",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let packet = demuxer
            .next_packet()
            .expect("compat next_packet не должен считать ResetRequired ошибкой")
            .expect("следующий packet нового track-а должен быть доступен");

        assert_eq!(packet.track_id, TrackId::new(3));
        assert_eq!(packet.kind, TrackKind::Audio);
    }

    #[test]
    fn accurate_demux_seek_uses_symphonia_accurate_mode() {
        assert_symphonia_seek_mode(
            DemuxSeekRequest::accurate(Duration::from_millis(500)),
            SeekMode::Accurate,
        );
    }

    #[test]
    fn decode_point_before_demux_seek_uses_symphonia_accurate_mode() {
        assert_symphonia_seek_mode(
            DemuxSeekRequest::decode_point_before(Duration::from_millis(500)),
            SeekMode::Accurate,
        );
    }

    #[test]
    fn preview_demux_seek_uses_symphonia_coarse_mode() {
        assert_symphonia_seek_mode(
            DemuxSeekRequest::preview(Duration::from_millis(500)),
            SeekMode::Coarse,
        );
    }

    #[test]
    fn preview_seek_clears_decode_point_prebuffer_without_verification() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![Ok(small_vp9_keyframe_packet(1, 400))],
                vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
            ])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "preview-clears-prebuffer",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("первый seek должен создать verification prebuffer");
        demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
            .expect("preview-mode seek не должен запускать DecodePointBefore verification");
        let packet = demuxer
            .next_packet()
            .expect("preview packet должен читаться после seek")
            .expect("preview-mode seek не должен съесть post-seek packet");

        assert_eq!(packet.pts, Duration::from_secs(11));
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .as_slice(),
            &[SeekMode::Accurate, SeekMode::Coarse]
        );
    }

    #[test]
    fn seek_result_uses_selected_video_track_timestamp_when_audio_track_is_first() {
        let reader = FakeFormatReader::new(
            vec![
                aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
                vp9_video_track(1),
            ],
            Vec::new(),
        );
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "selected-video-track-timestamp",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_secs(10)))
            .expect("preview-mode seek должен использовать selected video track");

        assert_eq!(
            seek_result
                .actual_track_timestamp
                .expect("seek result должен сохранить raw timestamp")
                .track_id,
            TrackId::new(1)
        );
    }

    #[test]
    fn decode_point_before_seek_rejects_coarse_after_target_position() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 0))]])
            .with_seek_response_policy(FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "coarse-after-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");
        let target = Duration::from_millis(100);

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("decode-point seek должен завершиться без ошибки");

        assert!(
            seek_result.actual_position.as_duration() <= target,
            "DecodePointBefore не должен принимать backend-позицию после target"
        );
    }

    #[test]
    fn decode_point_before_seek_retries_when_first_video_packet_overshoots_target() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![Ok(small_vp9_keyframe_packet(1, 11_000))],
                vec![Ok(small_vp9_keyframe_packet(1, 4_000))],
            ])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "packet-after-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");
        let target = Duration::from_secs(10);

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("decode-point seek должен retry-нуться до packet перед target");

        assert!(
            seek_result.actual_position.as_duration() <= target,
            "retry должен вернуть packet-level actual position не позже target"
        );
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .as_slice(),
            &[SeekMode::Accurate, SeekMode::Accurate]
        );
    }

    #[test]
    fn decode_point_before_after_target_retry_expands_existing_preroll() {
        let issue = DecodePointBeforeVerificationIssue::FirstVideoAfterTarget {
            packet: DecodePointBeforeVideoPacket {
                pts: Duration::from_millis(29_233),
                track_pts: None,
                keyframe: PacketKeyframe::Keyframe,
            },
        };

        let retry_timestamp = decode_point_before_retry_timestamp_for_issue(
            Duration::from_millis(24_225),
            Duration::from_millis(29_225),
            issue,
            0,
            Duration::from_secs(5),
        )
        .expect("after-target packet должен дать retry timestamp");

        assert_eq!(
            retry_timestamp,
            Duration::from_millis(19_225),
            "retry должен расширить pre-roll, а не отступить только на маленький overshoot"
        );
    }

    #[test]
    fn decode_point_before_seek_fails_when_actual_is_before_but_video_packet_is_after_target() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
            .map(|_| vec![Ok(small_vp9_keyframe_packet(1, 11_000))])
            .collect::<Vec<_>>();
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(scripts)
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "actual-before-packet-after",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(10_000),
            ))
            .expect_err("video packet после target должен отклонить DecodePointBefore");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("verification failure должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::DecodePointBeforeVerificationFailed {
                reason: "first_video_after_target",
                ..
            }
        ));
        assert!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len()
                > 1
        );
    }

    #[test]
    fn decode_point_before_seek_success_prebuffers_verified_video_packet() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![Ok(small_vp9_keyframe_packet(1, 400))]]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "packet-before-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("keyframe packet до target должен принять DecodePointBefore");
        let packet = demuxer
            .next_packet()
            .expect("prebuffered packet должен читаться без ошибки")
            .expect("verification не должна терять packet");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(400)
        );
        assert_eq!(
            seek_result
                .actual_track_timestamp
                .expect("video packet raw timestamp должен обновить actual")
                .track_id,
            TrackId::new(1)
        );
        assert_eq!(packet.kind, TrackKind::Video);
        assert_eq!(packet.pts, Duration::from_millis(400));
        assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
    }

    #[test]
    fn decode_point_before_returns_verification_events_in_read_order() {
        let reader = FakeFormatReader::new(
            vec![
                vp9_video_track(1),
                aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
            ],
            Vec::new(),
        )
        .with_seek_packet_scripts(vec![vec![
            Ok(fake_packet(2, 100, b"audio".to_vec())),
            Ok(small_vp9_keyframe_packet(1, 400)),
        ]]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "verification-order",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("verification должен принять selected video packet");

        let first_event = demuxer
            .next_event()
            .expect("первый buffered event должен читаться");
        let second_event = demuxer
            .next_event()
            .expect("второй buffered event должен читаться");

        match first_event {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, TrackId::new(2));
                assert_eq!(packet.kind, TrackKind::Audio);
                assert_eq!(packet.pts, Duration::from_millis(100));
            }
            unexpected_event => panic!("ожидали audio packet, получили {unexpected_event:?}"),
        }
        match second_event {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, TrackId::new(1));
                assert_eq!(packet.kind, TrackKind::Video);
                assert_eq!(packet.pts, Duration::from_millis(400));
            }
            unexpected_event => panic!("ожидали video packet, получили {unexpected_event:?}"),
        }
    }

    #[test]
    fn decode_point_before_verifies_selected_video_track_not_any_video_track() {
        let reader =
            FakeFormatReader::new(vec![vp9_video_track(1), vp9_video_track(2)], Vec::new())
                .with_seek_packet_scripts(vec![vec![
                    Ok(small_vp9_keyframe_packet(2, 11_000)),
                    Ok(small_vp9_keyframe_packet(1, 400)),
                ]]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "selected-video-verification",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("unselected video packet не должен решать verification selected track-а");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(400)
        );
        assert_eq!(
            seek_result
                .actual_track_timestamp
                .expect("verified actual должен быть timestamp selected video track-а")
                .track_id,
            TrackId::new(1)
        );
    }

    #[test]
    fn decode_point_before_seek_accepts_unknown_keyframe_before_target() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![Ok(fake_packet(1, 400, b"\x00".to_vec()))]]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "unknown-keyframe-before-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("unknown keyframe до target не должен блокировать seek полностью");
        let packet = demuxer
            .next_packet()
            .expect("prebuffered packet должен читаться")
            .expect("packet должен вернуться pipeline");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(400)
        );
        assert_eq!(packet.keyframe, PacketKeyframe::Unknown);
    }

    #[test]
    fn reset_required_after_preview_seek_is_returned_as_tracks_changed_event() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![
                Err(SymphoniaError::ResetRequired),
                Ok(small_vp9_keyframe_packet(4, 600)),
            ]])
            .with_reset_track_update(vec![vp9_video_track(4)]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "post-seek-reset",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        demuxer
            .seek_with_request(DemuxSeekRequest::preview(Duration::from_millis(500)))
            .expect("preview-mode seek должен завершиться до post-seek read");

        let reset_event = demuxer
            .next_event()
            .expect("ResetRequired после seek должен стать lifecycle event");
        let packet_event = demuxer
            .next_event()
            .expect("packet после TracksChanged должен остаться доступным");

        match reset_event {
            DemuxReadEvent::TracksChanged(track_update) => {
                assert_eq!(track_update.tracks[0].id, TrackId::new(4));
            }
            unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
        }
        match packet_event {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, TrackId::new(4));
                assert_eq!(packet.pts, Duration::from_millis(600));
            }
            unexpected_event => {
                panic!("ожидали packet нового track-а, получили {unexpected_event:?}")
            }
        }
    }

    #[test]
    fn reset_required_during_decode_point_verification_is_buffered_before_packet() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![
                Err(SymphoniaError::ResetRequired),
                Ok(small_vp9_keyframe_packet(4, 400)),
            ]])
            .with_reset_track_update(vec![vp9_video_track(4)]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "verification-reset",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(500),
            ))
            .expect("verification должен пережить ResetRequired и принять новый video track");
        let reset_event = demuxer
            .next_event()
            .expect("TracksChanged должен вернуться перед verified packet-ом");
        let packet_event = demuxer
            .next_event()
            .expect("verified packet должен вернуться после TracksChanged");

        assert_eq!(
            seek_result
                .actual_track_timestamp
                .expect("verified actual должен принадлежать новому video track-у")
                .track_id,
            TrackId::new(4)
        );
        assert!(matches!(reset_event, DemuxReadEvent::TracksChanged(_)));
        match packet_event {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, TrackId::new(4));
                assert_eq!(packet.pts, Duration::from_millis(400));
            }
            unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
        }
    }

    #[test]
    fn tracks_changed_from_failed_decode_point_verification_survives_retry() {
        let seek_track_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![
                    Err(SymphoniaError::ResetRequired),
                    Ok(small_vp9_inter_frame_packet(4, 400)),
                ],
                vec![Ok(small_vp9_keyframe_packet(4, 300))],
            ])
            .with_reset_track_update(vec![
                aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000)),
                vp9_video_track(4),
            ])
            .with_seek_track_log(Arc::clone(&seek_track_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "verification-reset-before-retry",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(10_000),
            ))
            .expect("retry должен найти keyframe после ResetRequired");
        let reset_event = demuxer
            .next_event()
            .expect("TracksChanged из rejected attempt должен сохраниться");
        let packet_event = demuxer
            .next_event()
            .expect("verified packet успешной retry-попытки должен остаться доступен");

        match reset_event {
            DemuxReadEvent::TracksChanged(track_update) => {
                assert!(
                    track_update
                        .tracks
                        .iter()
                        .any(|track| track.kind == TrackKind::Video && track.id == TrackId::new(4))
                );
            }
            unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
        }
        match packet_event {
            DemuxReadEvent::Packet(packet) => {
                assert_eq!(packet.track_id, TrackId::new(4));
                assert_eq!(packet.pts, Duration::from_millis(300));
                assert_eq!(packet.keyframe, PacketKeyframe::Keyframe);
            }
            unexpected_event => panic!("ожидали verified packet, получили {unexpected_event:?}"),
        }
        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(300)
        );
        assert_eq!(
            seek_track_log
                .lock()
                .expect("seek track log mutex should not be poisoned")
                .as_slice(),
            &[1, 4]
        );
    }

    #[test]
    fn decode_point_before_seek_retries_when_first_video_packet_is_not_keyframe() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![Ok(small_vp9_inter_frame_packet(1, 400))],
                vec![Ok(small_vp9_keyframe_packet(1, 300))],
            ])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "inter-frame-before-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(10_000),
            ))
            .expect("retry должен найти keyframe до target");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(300)
        );
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len(),
            2
        );
    }

    #[test]
    fn decode_point_before_accepts_keyframe_after_initial_inter_frame_in_prefix() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![vec![
                Ok(small_vp9_inter_frame_packet(1, 111_445)),
                Ok(small_vp9_keyframe_packet(1, 112_145)),
            ]])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "inter-frame-then-keyframe-before-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(116_449),
            ))
            .expect("verification должен принять keyframe внутри bounded prefix-а");
        let first_packet = demuxer
            .next_packet()
            .expect("prebuffered inter-frame должен читаться")
            .expect("verification prefix должен сохранить первый packet");
        let accepted_packet = demuxer
            .next_packet()
            .expect("prebuffered keyframe должен читаться")
            .expect("verification prefix должен сохранить accepted keyframe");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(112_145)
        );
        assert_eq!(first_packet.keyframe, PacketKeyframe::NotKeyframe);
        assert_eq!(accepted_packet.keyframe, PacketKeyframe::Keyframe);
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[test]
    fn decode_point_before_default_prefix_limit_reaches_later_keyframe() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let mut long_prefix = Vec::new();
        for frame_index in 0..180 {
            long_prefix.push(Ok(small_vp9_inter_frame_packet(
                1,
                65_181 + frame_index * 16,
            )));
        }
        long_prefix.push(Ok(small_vp9_keyframe_packet(1, 68_061)));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![long_prefix])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "long-prefix-keyframe-before-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(69_143),
            ))
            .expect("default verification limit должен дойти до keyframe текущего GOP");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(68_061)
        );
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len(),
            1
        );
    }

    #[test]
    fn decode_point_before_retries_when_prefix_reaches_target_before_keyframe() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![
                    Ok(small_vp9_inter_frame_packet(1, 9_900)),
                    Ok(small_vp9_inter_frame_packet(1, 10_100)),
                ],
                vec![Ok(small_vp9_keyframe_packet(1, 9_000))],
            ])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "inter-frame-crosses-target-before-keyframe",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(10_000),
            ))
            .expect("after-target packet до keyframe должен вызвать retry");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(9_000)
        );
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len(),
            2
        );
    }

    #[test]
    fn decode_point_before_retries_when_first_video_packet_is_too_far_before_target() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![Ok(small_vp9_keyframe_packet(1, 0))],
                vec![Ok(small_vp9_keyframe_packet(1, 96_000))],
            ])
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "too-far-before-target-rescue",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(96_784),
            ))
            .expect("too-far decode point должен retry-нуться ближе к target");

        assert_eq!(
            seek_result.actual_position,
            media_core::MediaTime::from_millis(96_000)
        );
        assert_eq!(
            seek_mode_log
                .lock()
                .expect("seek mode log mutex should not be poisoned")
                .len(),
            2
        );
    }

    #[test]
    fn decode_point_before_fails_instead_of_accepting_start_of_file_for_middle_seek() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_packet_scripts(vec![
                vec![Ok(small_vp9_keyframe_packet(1, 0))],
                vec![Ok(small_vp9_keyframe_packet(1, 0))],
            ]);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "too-far-before-target-failure",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(96_784),
            ))
            .expect_err("seek в середину файла не должен принимать packet с начала файла");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("too-far verification failure должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::DecodePointBeforeVerificationFailed {
                reason: "first_video_too_far_before_target",
                ..
            }
        ));
    }

    #[test]
    fn decode_point_before_uses_video_packet_when_audio_actual_is_earlier() {
        let scripts = (0..=DECODE_POINT_BEFORE_MAX_RETRIES)
            .map(|_| {
                vec![
                    Ok(fake_packet(1, 100, b"audio".to_vec())),
                    Ok(small_vp9_keyframe_packet(2, 11_000)),
                ]
            })
            .collect::<Vec<_>>();
        let reader = FakeFormatReader::new(
            vec![
                aac_audio_track_with_timing(1, SymphoniaDuration::new(30_000)),
                vp9_video_track(2),
            ],
            Vec::new(),
        )
        .with_seek_packet_scripts(scripts);
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "audio-actual-video-after",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");

        let error = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(
                Duration::from_millis(10_000),
            ))
            .expect_err("ранний audio actual не должен маскировать video packet после target");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("video verification failure должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::DecodePointBeforeVerificationFailed {
                reason: "first_video_after_target",
                ..
            }
        ));
    }

    #[test]
    fn normal_eof_returns_none_without_error() {
        let mut demuxer =
            fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());

        let packet = demuxer
            .next_packet()
            .expect("normal EOF не должен быть ошибкой");

        assert!(packet.is_none());
    }

    #[test]
    fn seek_after_eof_rebuilds_seekable_reader_and_preserves_track_layout() {
        let mut demuxer = SymphoniaDemuxer::from_file(&audio_fixture_path("music_sample.m4a"))
            .expect("seekable m4a fixture должен открыться");
        let tracks_before_eof = demuxer.tracks().to_vec();
        let duration_before_eof = demuxer.duration();

        drain_demuxer_to_eof_for_unit_test(&mut demuxer);

        assert!(demuxer.end_of_stream_reached);
        assert!(demuxer.format.is_some());

        demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect("seek after EOF должен rebuild-ить seekable reader");

        assert!(!demuxer.end_of_stream_reached);
        assert_eq!(demuxer.tracks(), tracks_before_eof.as_slice());
        assert_eq!(demuxer.duration(), duration_before_eof);
        assert!(demuxer.format.is_some());

        assert_first_packet_after_seek_for_unit_test(&mut demuxer);
    }

    #[test]
    fn unseekable_stream_seek_after_eof_does_not_rebuild_or_become_seekable() {
        let fixture_file = File::open(audio_fixture_path("music_sample.mp3"))
            .expect("mp3 fixture должен открыться как reader");
        let mut demuxer = SymphoniaDemuxer::from_stream(fixture_file, "mp3", "unit unseekable mp3")
            .expect("unseekable stream fixture должен открыться");
        let tracks_before_eof = demuxer.tracks().to_vec();
        let duration_before_eof = demuxer.duration();

        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable
            }
        ));

        drain_demuxer_to_eof_for_unit_test(&mut demuxer);

        let seek_error = demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect_err("unseekable stream не должен превращаться в seekable после EOF");

        assert!(
            seek_error.downcast_ref::<MediaDemuxError>().is_some()
                || seek_error.downcast_ref::<DemuxError>().is_some(),
            "seek failure должен остаться typed demux error: {seek_error}"
        );
        assert!(demuxer.end_of_stream_reached);
        assert!(demuxer.format.is_some());
        assert_eq!(demuxer.tracks(), tracks_before_eof.as_slice());
        assert_eq!(demuxer.duration(), duration_before_eof);
        assert!(matches!(
            demuxer.seekability(),
            DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable
            }
        ));
    }

    #[test]
    fn seek_reprobe_preserves_pending_tracks_changed_event() {
        let mut demuxer = SymphoniaDemuxer::from_file(&audio_fixture_path("music_sample.m4a"))
            .expect("seekable m4a fixture должен открыться");

        drain_demuxer_to_eof_for_unit_test(&mut demuxer);

        let retained_update =
            DemuxTrackListUpdate::new(demuxer.tracks().to_vec(), demuxer.duration());
        demuxer
            .pending_events
            .push_back(DemuxReadEvent::TracksChanged(retained_update.clone()));

        demuxer
            .seek_with_request(DemuxSeekRequest::accurate(Duration::ZERO))
            .expect("seek after EOF должен сохранить pending lifecycle event");

        match demuxer
            .next_event()
            .expect("retained TracksChanged должен читаться после seek")
        {
            DemuxReadEvent::TracksChanged(actual_update) => {
                assert_eq!(actual_update, retained_update);
            }
            unexpected_event => {
                panic!("ожидали retained TracksChanged, получили {unexpected_event:?}")
            }
        }

        assert_first_packet_after_seek_for_unit_test(&mut demuxer);
    }

    #[test]
    fn unknown_track_does_not_break_open() {
        let reader = FakeFormatReader::new(vec![unknown_track(9)], Vec::new());
        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("unknown track не должен ломать open");

        assert!(demuxer.tracks().is_empty());
    }

    #[test]
    fn subtitle_packets_are_skipped_without_unknown_track_error() {
        let reader = FakeFormatReader::new(
            vec![subtitle_track(9)],
            vec![Ok(fake_packet(9, 0, b"subtitle".to_vec()))],
        );
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("subtitle track не должен ломать open");

        let packet = demuxer
            .next_packet()
            .expect("subtitle packet должен быть пропущен без fatal ошибки");

        assert!(packet.is_none());
    }

    #[test]
    fn unexpected_eof_error_is_kept_as_defensive_eof_fallback() {
        let mut demuxer = fake_demuxer_with_options(
            vec![Err(SymphoniaError::IoError(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "legacy eof",
            )))],
            HashMap::new(),
            DemuxerOptions::default(),
        );

        let packet = demuxer
            .next_packet()
            .expect("defensive UnexpectedEof fallback должен остаться EOF");

        assert!(packet.is_none());
    }

    #[test]
    fn decode_error_from_format_reader_is_parse_error_without_retry() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(2)
            .expect("test limit ненулевой");
        let next_packet_call_count = Arc::new(Mutex::new(0));
        let reader = FakeFormatReader::new(
            vec![vp9_video_track(1)],
            vec![
                Err(SymphoniaError::DecodeError("isomp4: no atom pending read")),
                Ok(small_vp9_keyframe_packet(1, 10)),
            ],
        )
        .with_next_packet_call_count(next_packet_call_count.clone());
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "fake",
            HashMap::new(),
            DemuxSeekability::Seekable,
            options,
        )
        .expect("fake demuxer должен открыться");

        let error = demuxer
            .next_packet()
            .expect_err("structural DecodeError из next_packet должен быть fatal");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("fatal должен остаться typed DemuxError");

        match demux_error {
            DemuxError::Parse(SymphoniaError::DecodeError(reason)) => {
                assert_eq!(*reason, "isomp4: no atom pending read");
            }
            unexpected_error => panic!("ожидали Parse(DecodeError), получили {unexpected_error:?}"),
        }

        assert_eq!(
            *next_packet_call_count
                .lock()
                .expect("next_packet call count mutex should not be poisoned"),
            1
        );
    }

    #[test]
    fn packet_for_unknown_track_is_fatal() {
        let mut demuxer = fake_demuxer_with_options(
            vec![Ok(fake_packet(99, 0, b"\x00".to_vec()))],
            HashMap::new(),
            DemuxerOptions::default(),
        );

        let error = demuxer
            .next_packet()
            .expect_err("unknown track должен быть fatal");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("fatal должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::UnknownPacketTrack { track_id: 99 }
        ));
    }

    #[test]
    fn uncertain_vp9_keyframe_probe_is_returned_without_demux_error() {
        let matroska_tracks = HashMap::from([(
            TrackId::new(1),
            MatroskaVideoTrack {
                codec_id: Some("V_VP9".to_string()),
                metadata: None,
            },
        )]);
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Ok(fake_packet(1, 0, b"\x00".to_vec())),
                Ok(fake_packet(1, 10, b"\x00".to_vec())),
                Ok(small_vp9_keyframe_packet(1, 20)),
            ],
            matroska_tracks,
            DemuxerOptions::default(),
        );

        let first_packet = demuxer
            .next_packet()
            .expect("неуверенная keyframe-проба не должна становиться fatal corruption")
            .expect("packet с неизвестным keyframe должен быть возвращён");
        let second_packet = demuxer
            .next_packet()
            .expect("повторная неуверенная keyframe-проба не должна копить corruption counter")
            .expect("второй packet с неизвестным keyframe должен быть возвращён");

        assert_eq!(first_packet.pts, Duration::ZERO);
        assert_eq!(first_packet.keyframe, PacketKeyframe::Unknown);
        assert_eq!(second_packet.pts, Duration::from_millis(10));
        assert_eq!(second_packet.keyframe, PacketKeyframe::Unknown);
    }

    #[test]
    fn decode_point_before_seek_starts_video_before_target() {
        let mut demuxer =
            SymphoniaDemuxer::from_file(&test_webm_path()).expect("test webm должен открыться");
        let target = Duration::from_millis(500);

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("decode-point seek должен завершиться без ошибки");
        let packet = next_video_packet(&mut demuxer);

        assert_eq!(
            seek_result.requested_position,
            media_core::MediaTime::from_duration(target)
        );
        assert!(
            seek_result.actual_position.as_duration() <= target,
            "container должен поставить чтение не позже user target"
        );
        assert!(
            packet.pts <= target,
            "первый video packet после bootstrap seek должен быть pre-roll кадром"
        );
        // Keyframe flag теперь восстанавливается только codec-aware packet mapper-ом:
        // Symphonia 0.6 больше не отдаёт container keyframe flag в public Packet.
    }

    #[test]
    fn decode_point_before_seek_actual_position_stays_before_video_targets() {
        let targets = [
            Duration::ZERO,
            Duration::from_secs(106),
            Duration::from_millis(212_500),
        ];

        for target in targets {
            let mut demuxer =
                SymphoniaDemuxer::from_file(&test_webm_path()).expect("test webm должен открыться");

            let seek_result = demuxer
                .seek_with_request(DemuxSeekRequest::decode_point_before(target))
                .expect("decode-point seek должен завершиться без ошибки");

            assert!(
                seek_result.actual_position.as_duration() <= target,
                "actual demux position {:?} не должна быть после requested target {:?}",
                seek_result.actual_position.as_duration(),
                target
            );
        }
    }
}
