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
use source_core::ByteSource;
use tracing::{info, trace};

mod decode_point_before;
mod matroska_source_probe;
pub(crate) mod metadata;
mod seek_execution;

use crate::byte_source::ByteSourceMediaSource;
use crate::error::DemuxError;
use crate::isomp4_source_offset::{PacketSourceOffsetObserver, open_reader_with_source_offsets};
use crate::matroska_metadata::{MatroskaCueIndex, MatroskaVideoTrack};
use crate::options::DemuxerOptions;
use crate::packet_mapper::{PacketConvertError, convert_packet_with_source_offset};
use crate::seek_mapper::{preferred_seek_track_id, symphonia_seek_mode};
use crate::symphonia_api::{
    self, FormatReaderBox, Hint, MediaSourceStream, ReadOnlySource, SymphoniaError,
};
use crate::track_mapper::{TrackEntry, map_tracks_with_video_metadata};

use self::decode_point_before::prepend_retained_lifecycle_events;
use self::matroska_source_probe::{
    extension_may_have_matroska_video_metadata as matroska_extension_may_have_video_metadata,
    extract_cue_index_from_byte_source as probe_cue_index_from_byte_source,
    extract_cue_index_from_file_if_needed as probe_cue_index_from_file_if_needed,
    extract_video_tracks_from_byte_source as probe_video_tracks_from_byte_source,
    extract_video_tracks_from_file_if_needed as probe_video_tracks_from_file_if_needed,
    read_stream_prefix as read_matroska_stream_prefix,
    source_seekability_to_demux_seekability as map_source_seekability,
};

use self::metadata::{
    display_orientations_from_metadata, summarize_symphonia_format_metadata,
    video_color_metadata_from_metadata, video_packet_framings_from_metadata,
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
        let video_tracks_by_track = probe_video_tracks_from_file_if_needed(path, format.tracks());
        let matroska_cue_index = probe_cue_index_from_file_if_needed(path);

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
            if matroska_extension_may_have_video_metadata(extension_hint) {
                let mut reader = reader;
                let (stream_prefix, video_tracks_by_track) =
                    read_matroska_stream_prefix(&mut reader)?;
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
        let demux_seekability = map_source_seekability(source_seekability);
        let video_tracks_by_track = if matroska_extension_may_have_video_metadata(extension_hint) {
            probe_video_tracks_from_byte_source(&mut source, label)?
        } else {
            trace!(
                source = %label,
                extension_hint,
                "Matroska video metadata pre-scan skipped for non-Matroska byte source"
            );
            HashMap::new()
        };
        let matroska_cue_index = if matroska_extension_may_have_video_metadata(extension_hint) {
            probe_cue_index_from_byte_source(&mut source, label)?
        } else {
            MatroskaCueIndex::default()
        };
        let (media_source, failure_observer) =
            ByteSourceMediaSource::new_observed(Box::new(source));
        let media_source_stream =
            MediaSourceStream::new(Box::new(media_source), Default::default());

        let hint = symphonia_api::hint_from_extension(extension_hint);
        let format = symphonia_api::probe_format_reader(&hint, media_source_stream)
            .map_err(|error| failure_observer.take_demux_error().unwrap_or(error))?;
        failure_observer.finish_probe_success();

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
        let demux_seekability = map_source_seekability(source.seekability());
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
    let packet_framings_by_track = video_packet_framings_from_metadata(format);
    let track_mapping = map_tracks_with_video_metadata(
        format.tracks(),
        &mut video_tracks_for_mapping,
        &display_orientations_by_track,
        &color_metadata_by_track,
        &packet_framings_by_track,
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

#[cfg(test)]
mod tests;
