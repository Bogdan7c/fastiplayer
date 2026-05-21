use std::time::{Duration, Instant};

use media_core::{DemuxSeekRequest, MediaTime};

use crate::seek_controller::PlaybackResumeIntent;
use crate::{ScrubGeneration, SeekMode};

/// Тип seek transaction-а: финальный commit меняет playback position, preview только показывает кадр.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekCommitKind {
    /// Обычный seek или завершение scrub-а с фиксацией позиции.
    Final,

    /// Live preview во время активного scrub-а без закрытия scrub state.
    Preview,
}

/// Runtime state одного commit seek-а внутри playback pipeline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SeekCommitState {
    /// Поколение packets/frames, валидное для этой операции.
    pub generation: u64,

    /// Пользовательский scrub intent, если transaction родился из interactive scrub.
    pub scrub_generation: Option<ScrubGeneration>,

    /// Цель commit-а на нормализованной media timeline.
    pub target_position: MediaTime,

    /// Фактическая позиция, на которую container переставил demuxer.
    pub actual_position: MediaTime,

    /// Момент старта операции для timeout policy.
    pub started_at: Instant,

    /// Playback-состояние, которое нужно применить после прохождения gates.
    pub resume_intent: PlaybackResumeIntent,

    /// Поведение завершения commit-а.
    pub kind: SeekCommitKind,
}

impl SeekCommitState {
    /// Перепривязывает active seek к новому packet generation после container reset.
    ///
    /// `TracksChanged` не является новым пользовательским seek-ом: target, actual,
    /// scrub intent, timeout и resume policy остаются частью той же transaction.
    #[must_use]
    pub(crate) const fn rebased_to_generation(self, generation: u64) -> Self {
        Self { generation, ..self }
    }
}

/// Ошибка выбора container-level seek request-а до изменения runtime pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SeekDemuxRequestError {
    /// Запрошенный public `SeekMode` пока не имеет честной реализации в demux contract.
    UnsupportedSeekMode {
        /// Режим из пользовательской команды, который нельзя молча заменить другим.
        mode: SeekMode,
    },
}

/// Выбирает demux seek mode для текущего seek transaction-а.
///
/// Для video `SeekMode::Accurate` остаётся точным на уровне player-core:
/// demuxer начинает с decode-safe точки до target, а pre-roll/drop доводит
/// commit до исходной пользовательской позиции.
pub(crate) fn demux_seek_request_for_transaction(
    commit_kind: SeekCommitKind,
    has_video_track: bool,
    target_duration: Duration,
    seek_mode: SeekMode,
) -> Result<DemuxSeekRequest, SeekDemuxRequestError> {
    if seek_mode == SeekMode::KeyframeAfter {
        return Err(SeekDemuxRequestError::UnsupportedSeekMode { mode: seek_mode });
    }

    match commit_kind {
        SeekCommitKind::Preview => Ok(DemuxSeekRequest::preview(target_duration)),
        SeekCommitKind::Final => Ok(final_demux_seek_request(
            has_video_track,
            target_duration,
            seek_mode,
        )),
    }
}

/// Строит финальный demux request без потери public `SeekMode`.
fn final_demux_seek_request(
    has_video_track: bool,
    target_duration: Duration,
    seek_mode: SeekMode,
) -> DemuxSeekRequest {
    match seek_mode {
        SeekMode::Accurate if has_video_track => {
            DemuxSeekRequest::decode_point_before(target_duration)
        }
        SeekMode::Accurate => DemuxSeekRequest::accurate(target_duration),
        SeekMode::KeyframeBefore => DemuxSeekRequest::decode_point_before(target_duration),
        SeekMode::KeyframeAfter => unreachable!("KeyframeAfter rejected before final mapping"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use media_core::DemuxSeekMode;

    fn request_mode(
        commit_kind: SeekCommitKind,
        has_video_track: bool,
        seek_mode: SeekMode,
    ) -> Result<DemuxSeekMode, SeekDemuxRequestError> {
        demux_seek_request_for_transaction(
            commit_kind,
            has_video_track,
            Duration::from_millis(1_500),
            seek_mode,
        )
        .map(|request| request.mode)
    }

    #[test]
    fn accurate_audio_only_final_seek_stays_container_accurate() {
        let mode = request_mode(SeekCommitKind::Final, false, SeekMode::Accurate)
            .expect("audio-only accurate seek должен поддерживаться");

        assert_eq!(mode, DemuxSeekMode::Accurate);
    }

    #[test]
    fn accurate_video_final_seek_uses_decode_safe_preroll_request() {
        let mode = request_mode(SeekCommitKind::Final, true, SeekMode::Accurate)
            .expect("video accurate seek должен поддерживаться через preroll");

        assert_eq!(mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn keyframe_before_final_seek_maps_to_decode_point_before() {
        let mode = request_mode(SeekCommitKind::Final, true, SeekMode::KeyframeBefore)
            .expect("keyframe-before seek должен поддерживаться");

        assert_eq!(mode, DemuxSeekMode::DecodePointBefore);
    }

    #[test]
    fn preview_seek_uses_preview_demux_policy() {
        let mode = request_mode(SeekCommitKind::Preview, true, SeekMode::Accurate)
            .expect("preview seek должен поддерживаться");

        assert_eq!(mode, DemuxSeekMode::Preview);
    }

    #[test]
    fn keyframe_after_is_explicitly_unsupported() {
        let error = request_mode(SeekCommitKind::Final, true, SeekMode::KeyframeAfter)
            .expect_err("keyframe-after пока должен отклоняться явно");

        assert_eq!(
            error,
            SeekDemuxRequestError::UnsupportedSeekMode {
                mode: SeekMode::KeyframeAfter,
            }
        );
    }
}
