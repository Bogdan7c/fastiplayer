use std::time::{Duration, Instant};

use media_core::MediaTime;
use webm_demux::DemuxSeekRequest;

use crate::seek_controller::PlaybackResumeIntent;

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

/// Выбирает demux seek mode для текущего seek transaction-а.
///
/// При video track после decoder flush нельзя начинать decode с inter-frame.
/// Поэтому demuxer должен поставить чтение на decode-safe точку до target, а
/// точность commit-а остаётся в player-core: pre-roll/drop доводит кадры до
/// исходной пользовательской позиции.
pub(crate) fn demux_seek_request_for_transaction(
    commit_kind: SeekCommitKind,
    has_video_track: bool,
    target_duration: Duration,
) -> DemuxSeekRequest {
    if has_video_track {
        return DemuxSeekRequest::decode_point_before(target_duration);
    }

    match commit_kind {
        SeekCommitKind::Final => DemuxSeekRequest::accurate(target_duration),
        SeekCommitKind::Preview => DemuxSeekRequest::preview(target_duration),
    }
}
