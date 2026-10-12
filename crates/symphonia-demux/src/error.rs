use std::path::PathBuf;
use std::time::Duration;

use demux_api::OrderedSegmentReadError;
use media_core::{DemuxSeekMode, PacketKeyframe};

use crate::OrderedSegmentLifecycleError;

/// Ошибки demuxer.
#[derive(Debug, thiserror::Error)]
pub enum DemuxError {
    #[error("File not found: {0}")]
    FileNotFound(PathBuf),

    #[error("Unsupported format: {0}")]
    UnsupportedFormat(String),

    #[error("No video tracks in the file.")]
    NoVideoTracks,

    #[error("No audio tracks in the file")]
    NoAudioTracks,

    #[error("Reading error: {0}")]
    Io(#[from] std::io::Error),

    /// Typed lifecycle violation finite ordered ISO BMFF source-а.
    #[error(transparent)]
    OrderedSegmentLifecycle(#[from] OrderedSegmentLifecycleError),

    /// Typed operational failure neutral ordered segment source-а.
    #[error(transparent)]
    OrderedSegmentSource(#[from] OrderedSegmentReadError),

    #[error("Parsing error: {0}")]
    Parse(#[from] symphonia::core::errors::Error),

    #[error("Inner Symphonia reader inaccessible during surgery {operation}")]
    ReaderUnavailable {
        /// Операция, для которой demuxer ожидал активный reader.
        operation: &'static str,
    },

    #[error(
        "Reprobe changed the public track layout to {label}: {before_snapshot}, {after_snapshot}"
    )]
    ReprobeChangedTrackLayout {
        /// Человекочитаемый label source-а для diagnostics.
        label: String,

        /// Снимок public tracks/duration до rebuild-а.
        before_snapshot: String,

        /// Снимок public tracks/duration после rebuild-а.
        after_snapshot: String,
    },

    #[error(
        "Too many corrupted packets in a row: skipped {skipped}, limit {limit}; last error: {last_error}"
    )]
    TooManyCorruptedPackets {
        /// Настроенный лимит последовательных corrupted packets.
        limit: usize,

        /// Сколько corrupted packets встретилось подряд фактически.
        skipped: usize,

        /// Последняя причина, которую demuxer посчитал corrupted packet.
        last_error: String,
    },

    #[error("Packet references unknown track id {track_id}")]
    UnknownPacketTrack {
        /// Track id из container packet-а.
        track_id: u32,
    },

    #[error("Demux reset required: dynamic track changes are not supported yet")]
    ResetRequired,

    #[error("Seek unavailable: {0}")]
    SeekUnavailable(String),

    #[error("Seek mode {mode:?} is not supported by this demuxer implementation.")]
    UnsupportedSeekMode {
        /// Container-level режим, который demuxer не умеет честно выполнить.
        mode: DemuxSeekMode,
    },

    #[error("Error seek: {0}")]
    SeekFailed(String),

    #[error(
        "DecodePointBefore verification failed: reason={reason}, requested={requested_position:?}, attempts={attempts}, packets_checked={packets_checked}, first_video_pts={first_video_pts:?}, first_video_keyframe={first_video_keyframe:?}"
    )]
    DecodePointBeforeVerificationFailed {
        /// Краткая стабильная причина для diagnostics и тестов.
        reason: &'static str,

        /// Исходная позиция, которую запросил player.
        requested_position: Duration,

        /// Сколько backend seek попыток было сделано.
        attempts: usize,

        /// Сколько supported packets было проверено в последней попытке.
        packets_checked: usize,

        /// PTS первого selected video packet-а, если demuxer успел его увидеть.
        first_video_pts: Option<Duration>,

        /// Keyframe-классификация первого selected video packet-а, если он был найден.
        first_video_keyframe: Option<PacketKeyframe>,
    },

    /// Concrete finite FormatReader path не должен публиковать live readiness.
    #[error("Finite Symphonia seek verification unexpectedly received temporary readiness")]
    UnexpectedTemporaryReadinessDuringSeekVerification,
}

/// Поднимает typed ordered-input error из `io::Error`, не меняя обычный I/O path.
pub(crate) fn preserve_ordered_input_error(error: std::io::Error) -> DemuxError {
    if let Some(inner_error) = error.get_ref() {
        if let Some(lifecycle_error) = inner_error.downcast_ref::<OrderedSegmentLifecycleError>() {
            return lifecycle_error.clone().into();
        }
        if let Some(source_error) = inner_error.downcast_ref::<OrderedSegmentReadError>() {
            return source_error.clone().into();
        }
    }

    DemuxError::Io(error)
}

impl DemuxError {
    /// Возвращает `true`, если ошибка означает отсутствие seek capability.
    #[must_use]
    pub fn is_seek_unavailable(&self) -> bool {
        matches!(
            self,
            Self::SeekUnavailable(_) | Self::UnsupportedSeekMode { .. }
        )
    }
}
