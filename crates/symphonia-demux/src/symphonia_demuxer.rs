use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Read};
use std::path::Path;
use std::time::Duration;

use anyhow::Result;
use media_core::{
    DemuxReadEvent, DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DemuxSeekability,
    DemuxTrackListUpdate, Demuxer, Packet as OurPacket, TimelineNotSeekableReason, TrackId,
    TrackInfo,
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
    self, FormatReaderBox, MediaSourceStream, ReadOnlySource, SymphoniaError,
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

/// Demuxer на базе Symphonia для media containers, которые поддерживает workspace dependency.
pub struct SymphoniaDemuxer {
    format: FormatReaderBox<'static>,
    tracks: Vec<TrackInfo>,
    duration: Option<Duration>,
    track_map: HashMap<u32, TrackEntry>,
    matroska_video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
    seekability: DemuxSeekability,
    options: DemuxerOptions,
    consecutive_corrupted_packets: usize,
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

        Self::from_format_reader(
            format,
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

        Self::from_format_reader(
            format,
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

        Self::from_format_reader(
            format,
            label,
            video_tracks_by_track,
            demux_seekability,
            options,
        )
    }

    /// Собирает metadata и track map из готового Symphonia format reader.
    fn from_format_reader(
        mut format: FormatReaderBox<'static>,
        label: &str,
        video_tracks_by_track: HashMap<TrackId, MatroskaVideoTrack>,
        seekability: DemuxSeekability,
        options: DemuxerOptions,
    ) -> Result<Self, DemuxError> {
        let symphonia_metadata = summarize_symphonia_format_metadata(&mut format);
        let mut video_tracks_for_mapping = video_tracks_by_track.clone();
        let track_mapping = map_tracks(format.tracks(), &mut video_tracks_for_mapping);

        info!(
            source = %label,
            tracks = track_mapping.tracks.len(),
            duration = ?track_mapping.duration,
            attachments = symphonia_metadata.attachments,
            chapters = symphonia_metadata.has_chapters,
            metadata_revision = symphonia_metadata.has_metadata_revision,
            "Symphonia media source открыт"
        );

        Ok(Self {
            format,
            tracks: track_mapping.tracks,
            duration: track_mapping.duration,
            track_map: track_mapping.track_map,
            matroska_video_tracks_by_track: video_tracks_by_track,
            seekability,
            options,
            consecutive_corrupted_packets: 0,
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

    /// Сбрасывает счётчик corrupted packets после доказанного нормального продвижения.
    fn record_successful_packet(&mut self) {
        self.consecutive_corrupted_packets = 0;
    }

    /// Учитывает recoverable corrupted packet и возвращает fatal после configured лимита.
    fn record_corrupted_packet(
        &mut self,
        track_id: Option<TrackId>,
        reason: impl Into<String>,
    ) -> Result<()> {
        let reason = reason.into();
        self.consecutive_corrupted_packets = self.consecutive_corrupted_packets.saturating_add(1);
        let skipped = self.consecutive_corrupted_packets;
        let limit = self.options.max_consecutive_corrupted_packets();

        warn!(
            ?track_id,
            skipped,
            limit,
            reason = %reason,
            "Corrupted packet skipped"
        );

        if skipped <= limit {
            return Ok(());
        }

        Err(DemuxError::TooManyCorruptedPackets {
            limit,
            skipped,
            last_error: reason,
        }
        .into())
    }

    /// Перечитывает Symphonia track list после `ResetRequired` и обновляет demux boundary state.
    fn refresh_track_list_after_reset(&mut self) -> DemuxTrackListUpdate {
        let mut video_tracks_for_mapping = self.matroska_video_tracks_by_track.clone();
        let track_mapping = map_tracks(self.format.tracks(), &mut video_tracks_for_mapping);
        self.tracks = track_mapping.tracks;
        self.duration = track_mapping.duration;
        self.track_map = track_mapping.track_map;
        self.consecutive_corrupted_packets = 0;

        info!(
            tracks = self.tracks.len(),
            duration = ?self.duration,
            "Symphonia ResetRequired обработан как обновление track list"
        );

        DemuxTrackListUpdate::new(self.tracks.clone(), self.duration)
    }
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
        loop {
            match self.format.next_packet() {
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
                    return Ok(DemuxReadEvent::EndOfStream);
                }
                Err(SymphoniaError::IoError(ref e))
                    if e.kind() == std::io::ErrorKind::UnexpectedEof =>
                {
                    return Ok(DemuxReadEvent::EndOfStream);
                }
                Err(SymphoniaError::DecodeError(reason)) => {
                    self.record_corrupted_packet(None, reason)?;
                    continue;
                }
                Err(SymphoniaError::IoError(error)) => {
                    return Err(DemuxError::Io(error).into());
                }
                Err(SymphoniaError::ResetRequired) => {
                    let track_update = self.refresh_track_list_after_reset();
                    return Ok(DemuxReadEvent::TracksChanged(track_update));
                }
                Err(e) => {
                    return Err(DemuxError::Parse(e).into());
                }
            }
        }
    }

    fn seek(&mut self, timestamp: Duration) -> Result<DemuxSeekResult> {
        self.seek_with_request(DemuxSeekRequest::accurate(timestamp))
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> Result<DemuxSeekResult> {
        let seek_track_id = preferred_seek_track_id(&self.tracks);
        let seek_result = match request.mode {
            DemuxSeekMode::DecodePointBefore => {
                self.seek_decode_point_before(request, seek_track_id)?
            }
            DemuxSeekMode::Accurate | DemuxSeekMode::Preview => {
                self.seek_symphonia_once(request, seek_track_id, request.timestamp)?
            }
        };

        self.consecutive_corrupted_packets = 0;

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
    ) -> Result<DemuxSeekResult> {
        let backend_request = DemuxSeekRequest {
            timestamp: backend_timestamp,
            mode: request.mode,
        };
        let seek_mode = symphonia_seek_mode(request.mode);
        let seek_target = symphonia_seek_target(backend_request, seek_track_id);
        let seeked_to = self
            .format
            .seek(seek_mode, seek_target)
            .map_err(symphonia_seek_error_to_demux_error)?;

        Ok(seeked_to_timeline_result(
            request.timestamp,
            seeked_to,
            &self.track_map,
        ))
    }

    /// Восстанавливает `DecodePointBefore`: успешный result не должен быть после requested target.
    fn seek_decode_point_before(
        &mut self,
        request: DemuxSeekRequest,
        seek_track_id: Option<TrackId>,
    ) -> Result<DemuxSeekResult> {
        let requested_timestamp = request.timestamp;
        let mut backend_timestamp = decode_point_before_initial_timestamp(
            requested_timestamp,
            self.options.decode_point_before_preroll(),
        );

        for retry_index in 0..=DECODE_POINT_BEFORE_MAX_RETRIES {
            let seek_result =
                self.seek_symphonia_once(request, seek_track_id, backend_timestamp)?;
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

/// Создаёт ошибку, если backend не смог честно выполнить before-or-at-target seek.
fn decode_point_before_after_target_error(
    requested_timestamp: Duration,
    actual_timestamp: Duration,
    retry_index: usize,
) -> anyhow::Error {
    DemuxError::SeekFailed(format!(
        "DecodePointBefore вернул actual {actual_timestamp:?} после requested {requested_timestamp:?} после {attempts} попыток",
        attempts = retry_index + 1
    ))
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
    use std::io;
    use std::path::PathBuf;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use media_core::{
        DemuxReadEvent, DemuxSeekRequest, DemuxSeekability, Demuxer, PacketKeyframe, TrackId,
        TrackKind,
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
        MATROSKA_STREAM_SCAN_LIMIT_BYTES, MatroskaVideoMetadataScanDecision, SymphoniaDemuxer,
        decide_matroska_video_metadata_scan, read_stream_prefix,
    };
    use crate::error::DemuxError;
    use crate::matroska_metadata::MatroskaVideoTrack;
    use crate::options::DemuxerOptions;

    struct FakeFormatReader {
        format_info: FormatInfo,
        media_info: MediaInfo,
        tracks: Vec<Track>,
        reset_track_updates: VecDeque<Vec<Track>>,
        metadata: MetadataLog,
        packets: VecDeque<std::result::Result<Packet, SymphoniaError>>,
        seek_mode_log: Option<Arc<Mutex<Vec<SeekMode>>>>,
        seek_response_policy: FakeSeekResponsePolicy,
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum FakeSeekResponsePolicy {
        Zero,
        CoarseAfterTargetAccurateBefore,
        AlwaysAfterBackendTargetBySixSeconds,
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
                seek_mode_log: None,
                seek_response_policy: FakeSeekResponsePolicy::Zero,
            }
        }

        fn with_seek_mode_log(mut self, seek_mode_log: Arc<Mutex<Vec<SeekMode>>>) -> Self {
            self.seek_mode_log = Some(seek_mode_log);
            self
        }

        fn with_seek_response_policy(mut self, policy: FakeSeekResponsePolicy) -> Self {
            self.seek_response_policy = policy;
            self
        }

        fn with_reset_track_update(mut self, tracks: Vec<Track>) -> Self {
            self.reset_track_updates.push_back(tracks);
            self
        }

        fn seek_response(&self, mode: SeekMode, target: SeekTo) -> SeekedTo {
            let track_id = self
                .tracks
                .first()
                .map(|track| track.id)
                .unwrap_or_default();
            let required_ts = required_seek_timestamp(&self.tracks, target);
            let actual_ts = match self.seek_response_policy {
                FakeSeekResponsePolicy::Zero => Timestamp::ZERO,
                FakeSeekResponsePolicy::CoarseAfterTargetAccurateBefore => match mode {
                    SeekMode::Coarse => required_ts.saturating_add(SymphoniaDuration::new(250)),
                    SeekMode::Accurate => required_ts.saturating_sub(SymphoniaDuration::new(250)),
                },
                FakeSeekResponsePolicy::AlwaysAfterBackendTargetBySixSeconds => {
                    required_ts.saturating_add(SymphoniaDuration::new(6_000))
                }
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

            Ok(self.seek_response(mode, target))
        }

        fn tracks(&self) -> &[Track] {
            &self.tracks
        }

        fn next_packet(&mut self) -> symphonia::core::errors::Result<Option<Packet>> {
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

    fn required_seek_timestamp(tracks: &[Track], target: SeekTo) -> Timestamp {
        match target {
            SeekTo::Time { time, track_id } => track_id
                .and_then(|id| tracks.iter().find(|track| track.id == id))
                .or_else(|| tracks.first())
                .and_then(|track| track.time_base)
                .and_then(|time_base| time_base.calc_timestamp(time))
                .unwrap_or(Timestamp::ZERO),
            SeekTo::Timestamp { ts, .. } => ts,
        }
    }

    fn test_webm_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../test-assets/test.webm")
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
    fn decode_point_before_seek_rejects_coarse_after_target_position() {
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
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
    fn decode_point_before_seek_retries_when_accurate_backend_overshoots_target() {
        let seek_mode_log = Arc::new(Mutex::new(Vec::new()));
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_seek_response_policy(FakeSeekResponsePolicy::AlwaysAfterBackendTargetBySixSeconds)
            .with_seek_mode_log(Arc::clone(&seek_mode_log));
        let mut demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "accurate-after-target",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыться");
        let target = Duration::from_secs(10);

        let seek_result = demuxer
            .seek_with_request(DemuxSeekRequest::decode_point_before(target))
            .expect("decode-point seek должен retry-нуться до позиции перед target");

        assert!(
            seek_result.actual_position.as_duration() <= target,
            "retry должен вернуть actual demux position не позже target"
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
    fn normal_eof_returns_none_without_error() {
        let mut demuxer =
            fake_demuxer_with_options(Vec::new(), HashMap::new(), DemuxerOptions::default());

        let packet = demuxer
            .next_packet()
            .expect("normal EOF не должен быть ошибкой");

        assert!(packet.is_none());
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
    fn demuxer_stops_after_configured_corrupted_packet_limit() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(2)
            .expect("test limit ненулевой");
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Err(SymphoniaError::DecodeError("bad packet 1")),
                Err(SymphoniaError::DecodeError("bad packet 2")),
                Err(SymphoniaError::DecodeError("bad packet 3")),
            ],
            HashMap::new(),
            options,
        );

        let error = demuxer
            .next_packet()
            .expect_err("третья corrupted ошибка должна стать fatal");
        let demux_error = error
            .downcast_ref::<DemuxError>()
            .expect("fatal должен быть typed DemuxError");

        assert!(matches!(
            demux_error,
            DemuxError::TooManyCorruptedPackets {
                limit: 2,
                skipped: 3,
                ..
            }
        ));
    }

    #[test]
    fn successful_packet_resets_corrupted_packet_counter() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(2)
            .expect("test limit ненулевой");
        let mut demuxer = fake_demuxer_with_options(
            vec![
                Err(SymphoniaError::DecodeError("bad packet 1")),
                Err(SymphoniaError::DecodeError("bad packet 2")),
                Ok(small_vp9_keyframe_packet(1, 10)),
                Err(SymphoniaError::DecodeError("bad packet 3")),
                Err(SymphoniaError::DecodeError("bad packet 4")),
                Ok(small_vp9_keyframe_packet(1, 20)),
            ],
            HashMap::new(),
            options,
        );

        let first_packet = demuxer
            .next_packet()
            .expect("первое чтение должно пережить две corrupted ошибки")
            .expect("успешный packet должен быть возвращён");
        let second_packet = demuxer
            .next_packet()
            .expect("счётчик должен сброситься после первого packet")
            .expect("второй успешный packet должен быть возвращён");

        assert_eq!(first_packet.pts, Duration::from_millis(10));
        assert_eq!(second_packet.pts, Duration::from_millis(20));
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
    fn uncertain_vp9_keyframe_probe_is_returned_without_corruption_accounting() {
        let options = DemuxerOptions::from_max_consecutive_corrupted_packets(1)
            .expect("test limit ненулевой");
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
            options,
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
        assert_eq!(demuxer.consecutive_corrupted_packets, 0);
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
