use std::collections::HashMap;
use std::io::{self, Read};
use std::path::Path;

use media_core::{DemuxSeekability, TimelineNotSeekableReason, TrackId};
use source_core::{
    ByteSource, CancellationToken, Seekability as SourceSeekability, SourceError, SourceResult,
};
use tracing::{debug, trace, warn};

use crate::error::DemuxError;
use crate::matroska_metadata::{
    MATROSKA_CUES_SCAN_LIMIT_BYTES, MatroskaCueIndex, MatroskaVideoTrack,
    extract_cue_index_from_cues_bytes, extract_cue_index_from_file, extract_video_tracks_from_file,
    scan_cue_read_plan_from_bytes, scan_video_tracks_from_bytes,
};
use crate::track_mapper::tracks_may_need_matroska_video_metadata;

/// Верхняя граница prefix scan-а для seekable byte source-ов.
const MATROSKA_BYTE_SOURCE_SCAN_LIMIT_BYTES: usize = 4 * 1024 * 1024;

/// Более короткая граница для unseekable stream, чтобы open не ждал большой network prefix.
pub(super) const MATROSKA_STREAM_SCAN_LIMIT_BYTES: usize = 256 * 1024;

/// Решение о запуске Matroska/WebM scan после того, как Symphonia уже отдала track list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum MatroskaVideoMetadataScanDecision {
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

/// Конвертирует source seekability в neutral demux seekability.
pub(super) fn source_seekability_to_demux_seekability(
    seekability: SourceSeekability,
) -> DemuxSeekability {
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
pub(super) fn extract_video_tracks_from_file_if_needed(
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
pub(super) fn extract_cue_index_from_file_if_needed(path: &Path) -> MatroskaCueIndex {
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

/// Решает, нужен ли Matroska fallback для уже распробованного Symphonia reader-а.
pub(super) fn decide_matroska_video_metadata_scan(
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
pub(super) fn extension_may_have_matroska_video_metadata(extension_hint: &str) -> bool {
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
pub(super) fn extract_video_tracks_from_byte_source<S>(
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
pub(super) fn extract_cue_index_from_byte_source<S>(
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
pub(super) fn read_stream_prefix<R>(
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
