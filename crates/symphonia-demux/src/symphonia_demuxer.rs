use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, MediaDemuxError, TimelineNotSeekableReason, TrackId, TrackInfo,
};
use source_core::{
    ByteSource, CancellationToken, Seekability as SourceSeekability, SourceError, SourceResult,
};
use symphonia::core::units::Timestamp;
use tracing::{debug, info, trace, warn};

mod decode_point_before;
pub(crate) mod metadata;

use crate::byte_source::ByteSourceMediaSource;
use crate::error::DemuxError;
use crate::isomp4_source_offset::{PacketSourceOffsetObserver, open_reader_with_source_offsets};
use crate::matroska_metadata::{
    MATROSKA_CUES_SCAN_LIMIT_BYTES, MatroskaCueIndex, MatroskaVideoTrack,
    extract_cue_index_from_cues_bytes, extract_cue_index_from_file, extract_video_tracks_from_file,
    scan_cue_read_plan_from_bytes, scan_video_tracks_from_bytes,
};
use crate::options::DemuxerOptions;
use crate::packet_mapper::{PacketConvertError, convert_packet_with_source_offset};
use crate::seek_mapper::{
    preferred_seek_track_id, seeked_to_timeline_result, symphonia_seek_error_to_demux_error,
    symphonia_seek_mode, symphonia_seek_target,
};
use crate::symphonia_api::{
    self, FormatReaderBox, Hint, MediaSourceStream, ReadOnlySource, SeekErrorKind, SeekedTo,
    SymphoniaError, SymphoniaSeekMode,
};
use crate::track_mapper::{
    TrackEntry, map_tracks_with_video_metadata, tracks_may_need_matroska_video_metadata,
};

use self::metadata::{
    display_orientations_from_metadata, summarize_symphonia_format_metadata,
    video_color_metadata_from_metadata,
};

use self::decode_point_before::{
    DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN, DECODE_POINT_BEFORE_MAX_RETRIES,
    decode_point_before_after_target_error, decode_point_before_initial_timestamp,
    decode_point_before_retry_timestamp, decode_point_before_retry_timestamp_for_issue,
    decode_point_before_verification_error, log_decode_point_before_uncertainty,
    matroska_decode_point_before_retry_timestamp, prepend_retained_lifecycle_events,
    retain_tracks_changed_events_from_failed_verification, seek_result_with_verified_video_packet,
    selected_video_track_id,
};

#[cfg(test)]
use self::metadata::{
    RUSTIPLAYER_DISPLAY_ORIENTATION_CLOCKWISE_DEGREES_TAG, RUSTIPLAYER_VIDEO_COLOR_FULL_RANGE_TAG,
    RUSTIPLAYER_VIDEO_COLOR_MATRIX_COEFFICIENTS_H273_TAG,
    RUSTIPLAYER_VIDEO_COLOR_PRIMARIES_H273_TAG,
    RUSTIPLAYER_VIDEO_COLOR_TRANSFER_CHARACTERISTICS_H273_TAG,
    RUSTIPLAYER_VIDEO_HDR_MAX_CLL_NITS_TAG, RUSTIPLAYER_VIDEO_HDR_MAX_FALL_NITS_TAG,
    RUSTIPLAYER_VIDEO_HDR_MAX_LUMINANCE_NITS_TAG, RUSTIPLAYER_VIDEO_HDR_MIN_LUMINANCE_NITS_TAG,
};

/// Верхняя граница prefix scan-а для seekable byte source-ов.
const MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Более короткая граница для unseekable stream, чтобы open не ждал большой network prefix.
const MATROSKA_STREAM_SCAN_LIMIT_BYTES: usize = 256 * 1024;

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
    /// Присутствует только у concrete ISO-BMFF reader-а из доказанного registry route-а.
    packet_source_offset_observer: Option<PacketSourceOffsetObserver>,
    probe_hint: Hint,
    source_label: String,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    matroska_video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
    matroska_cue_index: MatroskaCueIndex,
    seekability: DemuxSeekability,
    options: DemuxerOptions,
    pending_events: VecDeque<DemuxReadEvent>,
    media_metadata: media_core::MediaMetadata,
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

/// Именованный context открытия отделяет reader ownership от source/probe state demuxer-а.
struct FormatReaderProbeContext {
    /// Hint нужен только для controlled reprobe того же container-а.
    probe_hint: Hint,
    /// Безопасный source label используется только в diagnostics.
    source_label: String,
    /// Matroska-only metadata остаётся пустой для concrete ISO-BMFF route-а.
    matroska_video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
    /// Matroska cue fallback не смешивается с ISO-BMFF source-position boundary.
    matroska_cue_index: MatroskaCueIndex,
    /// Seekability принадлежит source stack-у, а не выбранному reader adapter-у.
    seekability: DemuxSeekability,
    /// Observer присутствует только у concrete ISO-BMFF reader-а.
    packet_source_offset_observer: Option<PacketSourceOffsetObserver>,
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
        let matroska_cue_index = extract_cue_index_from_file_if_needed(path);

        Self::from_format_reader_with_probe_context(
            format,
            FormatReaderProbeContext {
                probe_hint: hint,
                source_label: path.display().to_string(),
                matroska_video_tracks_by_track: video_tracks_by_track,
                matroska_cue_index,
                seekability: DemuxSeekability::Seekable,
                packet_source_offset_observer: None,
            },
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
            FormatReaderProbeContext {
                probe_hint: hint,
                source_label: label.to_owned(),
                matroska_video_tracks_by_track: video_tracks_by_track,
                matroska_cue_index: MatroskaCueIndex::default(),
                seekability: DemuxSeekability::NotSeekable {
                    reason: TimelineNotSeekableReason::SourceNotSeekable,
                },
                packet_source_offset_observer: None,
            },
            options,
        )
    }

    /// Открывает уже доказанный registry-ем ISO-BMFF stream через concrete source-offset reader.
    pub(crate) fn from_proven_iso_bmff_stream_with_options<R>(
        reader: R,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        R: Read + Send + Sync + 'static,
    {
        let media_source = ReadOnlySource::new(reader);
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());
        Self::from_proven_iso_bmff_media_source_stream(
            media_source_stream,
            symphonia_api::hint_from_extension(extension_hint),
            label,
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
        let matroska_cue_index = if extension_may_have_matroska_video_metadata(extension_hint) {
            extract_cue_index_from_byte_source(&mut source, label)?
        } else {
            MatroskaCueIndex::default()
        };
        let media_source = ByteSourceMediaSource::new(Box::new(source));
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)?;

        Self::from_format_reader_with_probe_context(
            format,
            FormatReaderProbeContext {
                probe_hint: hint,
                source_label: label.to_owned(),
                matroska_video_tracks_by_track: video_tracks_by_track,
                matroska_cue_index,
                seekability: demux_seekability,
                packet_source_offset_observer: None,
            },
            options,
        )
    }

    /// Открывает уже доказанный registry-ем ISO-BMFF byte source с сохранением seekability.
    pub(crate) fn from_proven_iso_bmff_byte_source_with_options<S>(
        source: S,
        extension_hint: &str,
        label: &str,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError>
    where
        S: ByteSource + 'static,
    {
        let demux_seekability = source_seekability_to_demux_seekability(source.seekability());
        let media_source = ByteSourceMediaSource::new(Box::new(source));
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());
        Self::from_proven_iso_bmff_media_source_stream(
            media_source_stream,
            symphonia_api::hint_from_extension(extension_hint),
            label,
            demux_seekability,
            options,
        )
    }

    /// Собирает neutral demuxer вокруг concrete ISO-BMFF reader-а и его per-read observer-а.
    fn from_proven_iso_bmff_media_source_stream(
        media_source_stream: MediaSourceStream<'static>,
        probe_hint: Hint,
        label: &str,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let (format, packet_source_offset_observer) =
            open_reader_with_source_offsets(media_source_stream)
                .map_err(|error| DemuxError::UnsupportedFormat(error.to_string()))?;
        Self::from_format_reader_with_probe_context(
            format,
            FormatReaderProbeContext {
                probe_hint,
                source_label: label.to_owned(),
                matroska_video_tracks_by_track: HashMap::new(),
                matroska_cue_index: MatroskaCueIndex::default(),
                seekability,
                packet_source_offset_observer: Some(packet_source_offset_observer),
            },
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
            FormatReaderProbeContext {
                probe_hint: Hint::default(),
                source_label: label.to_owned(),
                matroska_video_tracks_by_track: video_tracks_by_track,
                matroska_cue_index: MatroskaCueIndex::default(),
                seekability,
                packet_source_offset_observer: None,
            },
            options,
        )
    }

    /// Собирает metadata и track map, сохраняя context для будущего controlled reprobe.
    fn from_format_reader_with_probe_context(
        mut format: FormatReaderBox<'static>,
        probe_context: FormatReaderProbeContext,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let FormatReaderProbeContext {
            probe_hint,
            source_label,
            matroska_video_tracks_by_track,
            matroska_cue_index,
            seekability,
            packet_source_offset_observer,
        } = probe_context;
        let symphonia_metadata = summarize_symphonia_format_metadata(&mut format);
        let track_state =
            track_state_from_format_reader(&mut format, &matroska_video_tracks_by_track);

        info!(
            source = %source_label,
            tracks = track_state.tracks.len(),
            duration = ?track_state.duration,
            track_duration = ?track_state.track_duration,
            media_info_duration = ?track_state.media_info_duration,
            attachments = symphonia_metadata.attachments,
            chapters = symphonia_metadata.has_chapters,
            metadata_revision = symphonia_metadata.has_metadata_revision,
            "Symphonia media source открыт"
        );

        let mut media_metadata = media_core::MediaMetadata::default();
        metadata::consume_media_metadata(&mut format, &mut media_metadata);
        Ok(Self {
            format: Some(format),
            packet_source_offset_observer,
            probe_hint,
            source_label,
            tracks: track_state.tracks,
            duration: track_state.duration,
            track_map: track_state.track_map,
            matroska_video_tracks_by_track,
            matroska_cue_index,
            seekability,
            options,
            pending_events: VecDeque::new(),
            media_metadata,
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

        let (mut rebuilt_format, rebuilt_packet_source_offset_observer) =
            if self.packet_source_offset_observer.is_some() {
                let (format, observer) = open_reader_with_source_offsets(media_source_stream)?;
                (format, Some(observer))
            } else {
                (
                    symphonia_api::probe_format_reader(&self.probe_hint, media_source_stream)?,
                    None,
                )
            };
        let symphonia_metadata = summarize_symphonia_format_metadata(&mut rebuilt_format);
        let track_state = track_state_from_format_reader(
            &mut rebuilt_format,
            &self.matroska_video_tracks_by_track,
        );
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
        self.packet_source_offset_observer = rebuilt_packet_source_offset_observer;
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
            let packet_source_offset = self
                .packet_source_offset_observer
                .as_ref()
                .and_then(PacketSourceOffsetObserver::take);

            match next_packet_result {
                Ok(Some(packet)) => match convert_packet_with_source_offset(
                    packet,
                    &self.track_map,
                    packet_source_offset,
                ) {
                    Ok(our_packet) => {
                        self.record_successful_packet();
                        let metadata_changed = {
                            let mut metadata_snapshot = self.media_metadata.clone();
                            let changed = metadata::consume_media_metadata(
                                self.format_mut("metadata")?,
                                &mut metadata_snapshot,
                            );
                            if changed {
                                self.media_metadata = metadata_snapshot;
                            }
                            changed
                        };
                        if metadata_changed {
                            self.pending_events
                                .push_back(DemuxReadEvent::Packet(our_packet));
                            return Ok(DemuxReadEvent::MediaMetadataChanged(
                                self.media_metadata.clone(),
                            ));
                        }
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
                    return Err(crate::error::preserve_ordered_input_error(error).into());
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

/// Строит neutral track state из текущего Symphonia reader-а без изменения public boundary.
fn track_state_from_format_reader(
    format: &mut FormatReaderBox<'static>,
    video_tracks_by_track: &HashMap<TrackId, MatroskaVideoTrack>,
) -> SymphoniaTrackState {
    let mut video_tracks_for_mapping = video_tracks_by_track.clone();
    let display_orientations_by_track = display_orientations_from_metadata(format);
    let color_metadata_by_track = video_color_metadata_from_metadata(format);
    let track_mapping = map_tracks_with_video_metadata(
        format.tracks(),
        &mut video_tracks_for_mapping,
        &display_orientations_by_track,
        &color_metadata_by_track,
    );
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

    fn media_metadata(&self) -> Option<media_core::MediaMetadata> {
        Some(self.media_metadata.clone())
    }

    fn seekability(&self) -> DemuxSeekability {
        self.seekability
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
        if let DemuxSeekability::NotSeekable { reason } = self.seekability {
            return Err(MediaDemuxError::SeekUnavailable {
                reason: format!("source/container boundary помечен как non-seekable: {reason:?}"),
            }
            .into());
        }

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
                    symphonia_seek_mode(request.mode),
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
        seek_mode: SymphoniaSeekMode,
        seek_track_id: Option<TrackId>,
        backend_timestamp: Duration,
        reprobe_before_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let backend_request = DemuxSeekRequest {
            timestamp: backend_timestamp,
            mode: request.mode,
        };
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
                    && backend_request.mode == DemuxSeekMode::DecodePointBefore
                    && backend_request.timestamp.is_zero()
                    && self.can_reprobe_current_source() =>
            {
                self.reset_decode_point_before_to_source_start(seek_track_id)
            }
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

    /// Возвращает seekable source к физическому началу для `DecodePointBefore(0)`.
    ///
    /// Некоторые Matroska reader-ы Symphonia отклоняют timestamp `0` как out-of-range,
    /// когда первый cluster/track timestamp начинается чуть позже нуля. Reprobe из
    /// source start выражает нужное намерение без выдуманного положительного timestamp;
    /// последующая packet verification по-прежнему проверяет keyframe и startup lead.
    fn reset_decode_point_before_to_source_start(
        &mut self,
        seek_track_id: Option<TrackId>,
    ) -> Result<SeekedTo> {
        self.rebuild_format_reader_from_source_start()?;
        Ok(SeekedTo {
            track_id: seek_track_id.map_or(0, TrackId::get),
            required_ts: Timestamp::ZERO,
            actual_ts: Timestamp::ZERO,
        })
    }

    /// Восстанавливает `DecodePointBefore`: успешный result не должен быть после requested target.
    fn seek_decode_point_before(
        &mut self,
        request: DemuxSeekRequest,
        reprobe_before_first_seek: bool,
    ) -> Result<DemuxSeekResult> {
        let requested_timestamp = request.timestamp;
        // RC1: целимся в сам target (минус крошечный margin), а не в target − preroll,
        // чтобы stss/cues приземлились на ближайший keyframe ≤ target. 5-секундный
        // `decode_point_before_preroll` ниже остаётся только шагом retry-backoff-а.
        let mut backend_timestamp = decode_point_before_initial_timestamp(
            requested_timestamp,
            DECODE_POINT_BEFORE_INITIAL_SEEK_MARGIN,
        );
        let mut backend_seek_mode = symphonia_seek_mode(request.mode);
        if let Some(video_track_id) = selected_video_track_id(&self.tracks) {
            let (matroska_backend_timestamp, uses_matroska_cue_anchor) =
                self.matroska_decode_point_before_anchor(video_track_id, backend_timestamp);
            backend_timestamp = matroska_backend_timestamp;
            if uses_matroska_cue_anchor {
                backend_seek_mode = SymphoniaSeekMode::Coarse;
            }
        }
        let mut retained_lifecycle_events = VecDeque::new();
        let mut minimum_video_timestamp = None;

        for retry_index in 0..=DECODE_POINT_BEFORE_MAX_RETRIES {
            let seek_track_id = preferred_seek_track_id(&self.tracks);
            let seek_result = self.seek_symphonia_once(
                request,
                backend_seek_mode,
                seek_track_id,
                backend_timestamp,
                retry_index == 0 && reprobe_before_first_seek,
            )?;

            if let Some(video_track_id) = selected_video_track_id(&self.tracks) {
                let verification = self.verify_decode_point_before_attempt(
                    requested_timestamp,
                    video_track_id,
                    minimum_video_timestamp,
                )?;

                if let Some(issue) = verification.issue {
                    let matroska_cue_retry_timestamp = matroska_decode_point_before_retry_timestamp(
                        &self.matroska_cue_index,
                        video_track_id,
                        backend_timestamp,
                        issue,
                    );
                    let retry_timestamp =
                        if let Some(cue_retry_timestamp) = matroska_cue_retry_timestamp {
                            minimum_video_timestamp.get_or_insert(backend_timestamp);
                            Some(cue_retry_timestamp)
                        } else {
                            decode_point_before_retry_timestamp_for_issue(
                                backend_timestamp,
                                requested_timestamp,
                                issue,
                                retry_index,
                                self.options.decode_point_before_preroll(),
                                self.options.decode_point_before_max_accepted_preroll(),
                            )
                        };
                    let Some(retry_timestamp) = retry_timestamp else {
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

                    let retry_uses_matroska_cue = matroska_cue_retry_timestamp.is_some();
                    debug!(
                        target_ms = requested_timestamp.as_millis(),
                        retry_ms = retry_timestamp.as_millis(),
                        retry_index,
                        reason = issue.reason(),
                        packets_checked = verification.packets_checked,
                        first_video_pts_ms = issue.first_video_pts().map(|pts| pts.as_millis()),
                        first_video_keyframe = ?issue.first_video_keyframe(),
                        retry_uses_matroska_cue,
                        "Post-seek verification rejected DecodePointBefore; retrying earlier"
                    );

                    retain_tracks_changed_events_from_failed_verification(
                        &mut retained_lifecycle_events,
                        verification.buffered_events,
                    );
                    backend_timestamp = retry_timestamp;
                    backend_seek_mode = if retry_uses_matroska_cue {
                        SymphoniaSeekMode::Coarse
                    } else {
                        symphonia_seek_mode(request.mode)
                    };
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

/// Конвертирует source seekability в neutral demux seekability.
fn source_seekability_to_demux_seekability(seekability: SourceSeekability) -> DemuxSeekability {
    match seekability {
        SourceSeekability::Seekable => DemuxSeekability::Seekable,
        SourceSeekability::NotSeekable { reason } => match reason {
            source_core::NotSeekableReason::HttpRangeStatus { .. }
            | source_core::NotSeekableReason::FtpRestUnsupported => DemuxSeekability::NotSeekable {
                reason: TimelineNotSeekableReason::SourceNotSeekable,
            },
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

/// Запускает Matroska cue pre-scan только там, где `DecodePointBefore` может выиграть от Cues.
fn extract_cue_index_from_file_if_needed(path: &Path) -> MatroskaCueIndex {
    let extension_hint = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    if !extension_may_have_matroska_video_metadata(extension_hint) {
        trace!(
            path = %path.display(),
            extension_hint,
            "Matroska cue pre-scan skipped for file"
        );
        return MatroskaCueIndex::default();
    }

    match extract_cue_index_from_file(path) {
        Ok(cue_index) => cue_index,
        Err(error) => {
            warn!(
                error = %error,
                path = %path.display(),
                "Matroska cue pre-scan failed"
            );
            MatroskaCueIndex::default()
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

/// Читает Matroska cues из seekable byte source-а и возвращает source cursor назад.
fn extract_cue_index_from_byte_source<S>(
    source: &mut S,
    label: &str,
) -> Result<MatroskaCueIndex, DemuxError>
where
    S: ByteSource,
{
    if !source.seekability().is_seekable() {
        debug!(
            source = %label,
            "Matroska cue byte-source pre-scan skipped for unseekable source"
        );
        return Ok(MatroskaCueIndex::default());
    }

    let original_position = source.position();
    let scan_result = read_byte_source_cue_index(source);
    let reset_result = source.seek(original_position);

    if let Err(error) = reset_result {
        return Err(source_error_to_demux_error(error));
    }

    match scan_result {
        Ok(cue_index) => Ok(cue_index),
        Err(error) => {
            warn!(
                error = %error,
                source = %label,
                "Matroska cue byte-source pre-scan failed"
            );
            Ok(MatroskaCueIndex::default())
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

/// Читает bounded cue index из seekable byte source-а.
fn read_byte_source_cue_index<S>(source: &mut S) -> SourceResult<MatroskaCueIndex>
where
    S: ByteSource,
{
    source.seek(0)?;
    let metadata_prefix = read_byte_source_prefix(source, MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES)?;
    let cue_plan = scan_cue_read_plan_from_bytes(&metadata_prefix);
    let mut cue_index = cue_plan.cue_index;

    if let Some(cues_absolute_position) = cue_plan.cues_absolute_position {
        source.seek(cues_absolute_position)?;
        let cues_prefix = read_byte_source_prefix(
            source,
            usize::try_from(MATROSKA_CUES_SCAN_LIMIT_BYTES).unwrap_or(usize::MAX),
        )?;
        if let Some(cues_index) =
            extract_cue_index_from_cues_bytes(&cues_prefix, cue_plan.timestamp_scale_ns)
        {
            cue_index.merge(cues_index);
        }
    }

    Ok(cue_index)
}

/// Читает bounded prefix из текущей позиции byte source-а.
fn read_byte_source_prefix<S>(source: &mut S, limit_bytes: usize) -> SourceResult<Vec<u8>>
where
    S: ByteSource,
{
    let cancellation = CancellationToken::never_cancelled();
    let mut prefix = Vec::new();
    let mut read_buffer = [0_u8; 64 * 1024];

    while prefix.len() < limit_bytes {
        let remaining_bytes = limit_bytes - prefix.len();
        let read_size = remaining_bytes.min(read_buffer.len());
        let bytes_read = source.read(&mut read_buffer[..read_size], &cancellation)?;

        if bytes_read == 0 {
            break;
        }

        prefix.extend_from_slice(&read_buffer[..bytes_read]);
    }

    Ok(prefix)
}

/// Конвертирует source-layer ошибку pre-scan-а в demux-level IO ошибку.
fn source_error_to_demux_error(error: SourceError) -> DemuxError {
    DemuxError::Io(io::Error::other(error))
}

#[cfg(test)]
mod tests;
