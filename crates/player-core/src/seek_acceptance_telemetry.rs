//! Минимальное session-owned состояние acceptance telemetry для final seek.
//!
//! Модуль ничего не знает о transport, demuxer, renderer или UI. Он только связывает
//! уже подтверждённый player lifecycle: принятую generation, реально presented frames,
//! commit и первый положительный post-commit clock sample.

use std::time::{Duration, Instant};

use crate::seek_state::SeekCommitState;

/// Доказательство первого target/post-target кадра текущего final seek-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekTargetPresentationEvidence {
    generation: u64,
    presented_pre_target_frames: u64,
}

impl SeekTargetPresentationEvidence {
    /// Возвращает seek generation, которой принадлежит кадр.
    #[must_use]
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    /// Возвращает число реально показанных pre-target кадров до первого target frame-а.
    #[must_use]
    pub(crate) const fn presented_pre_target_frames(self) -> u64 {
        self.presented_pre_target_frames
    }
}

/// Одноразовое доказательство, что playback clock продвинулся после seek commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SeekPositionProgressEvidence {
    generation: u64,
    target_position: Duration,
    committed_position: Duration,
    observed_position: Duration,
    public_elapsed: Duration,
    receipt_elapsed: Duration,
    commit_elapsed: Duration,
}

impl SeekPositionProgressEvidence {
    /// Возвращает seek generation, которую подтверждает clock sample.
    #[must_use]
    pub(crate) const fn generation(self) -> u64 {
        self.generation
    }

    /// Возвращает исходную public target position.
    #[must_use]
    pub(crate) const fn target_position(self) -> Duration {
        self.target_position
    }

    /// Возвращает позицию, опубликованную final commit-ом.
    #[must_use]
    pub(crate) const fn committed_position(self) -> Duration {
        self.committed_position
    }

    /// Возвращает первый строго больший post-commit clock sample.
    #[must_use]
    pub(crate) const fn observed_position(self) -> Duration {
        self.observed_position
    }

    /// Возвращает owner-monotonic время от accepted seek до progress proof.
    #[must_use]
    pub(crate) const fn public_elapsed(self) -> Duration {
        self.public_elapsed
    }

    /// Возвращает owner-monotonic время от authoritative receipt до progress proof.
    #[must_use]
    pub(crate) const fn receipt_elapsed(self) -> Duration {
        self.receipt_elapsed
    }

    /// Возвращает owner-monotonic время от commit до progress proof.
    #[must_use]
    pub(crate) const fn commit_elapsed(self) -> Duration {
        self.commit_elapsed
    }
}

/// Presentation-счётчики одной активной seek generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ActiveSeekPresentationProof {
    generation: u64,
    target_position: Duration,
    presented_pre_target_frames: u64,
    presented_frame_observed: bool,
    target_frame_presented: bool,
}

/// Ожидаемое одноразовое продвижение позиции после Playing commit-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingSeekPositionProgress {
    generation: u64,
    target_position: Duration,
    committed_position: Duration,
    public_accepted_at: Instant,
    receipt_accepted_at: Instant,
    committed_at: Instant,
}

/// Bounded telemetry state: максимум один active seek и один pending progress proof.
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SeekAcceptanceTelemetry {
    active_presentation: Option<ActiveSeekPresentationProof>,
    pending_position_progress: Option<PendingSeekPositionProgress>,
}

impl SeekAcceptanceTelemetry {
    /// Начинает доказательство для нового accepted seek-а и supersede-ит прежний progress proof.
    pub(crate) fn begin_seek(&mut self, seek_commit: SeekCommitState) {
        self.active_presentation = Some(ActiveSeekPresentationProof {
            generation: seek_commit.generation,
            target_position: seek_commit.target_position.as_duration(),
            presented_pre_target_frames: 0,
            presented_frame_observed: false,
            target_frame_presented: false,
        });
        self.pending_position_progress = None;
    }

    /// Перепривязывает доказательство к generation после authoritative topology reset-а.
    pub(crate) fn rebase_seek(&mut self, seek_commit: SeekCommitState) {
        let Some(active_presentation) = self.active_presentation.as_mut() else {
            self.begin_seek(seek_commit);
            return;
        };

        active_presentation.generation = seek_commit.generation;
        active_presentation.target_position = seek_commit.target_position.as_duration();
    }

    /// Учитывает только реально presented pre-target frame текущей generation.
    pub(crate) fn record_presented_pre_target_frame(
        &mut self,
        generation: u64,
        frame_pts: Duration,
    ) {
        let Some(active_presentation) = self.active_presentation.as_mut() else {
            return;
        };
        if active_presentation.generation != generation
            || frame_pts >= active_presentation.target_position
        {
            return;
        }

        active_presentation.presented_pre_target_frames = active_presentation
            .presented_pre_target_frames
            .saturating_add(1);
        active_presentation.presented_frame_observed = true;
    }

    /// Возвращает evidence ровно один раз для первого target/post-target presented frame-а.
    pub(crate) fn record_first_target_frame_presented(
        &mut self,
        generation: u64,
        frame_pts: Duration,
    ) -> Option<SeekTargetPresentationEvidence> {
        let active_presentation = self.active_presentation.as_mut()?;
        if active_presentation.generation != generation
            || frame_pts < active_presentation.target_position
            || active_presentation.target_frame_presented
        {
            return None;
        }

        active_presentation.presented_frame_observed = true;
        active_presentation.target_frame_presented = true;
        Some(SeekTargetPresentationEvidence {
            generation,
            presented_pre_target_frames: active_presentation.presented_pre_target_frames,
        })
    }

    /// Возвращает финальный counter только после хотя бы одного реального presentation.
    #[must_use]
    pub(crate) fn commit_presentation_evidence(&self, generation: u64) -> Option<u64> {
        let active_presentation = self.active_presentation?;
        (active_presentation.generation == generation
            && active_presentation.presented_frame_observed)
            .then_some(active_presentation.presented_pre_target_frames)
    }

    /// Завершает presentation accounting, не уничтожая уже armed progress proof.
    pub(crate) fn clear_active_presentation(&mut self) {
        self.active_presentation = None;
    }

    /// Взводит одноразовое доказательство продвижения после успешного Playing commit-а.
    pub(crate) fn arm_position_progress(
        &mut self,
        seek_commit: SeekCommitState,
        committed_position: Duration,
        committed_at: Instant,
    ) {
        self.pending_position_progress = Some(PendingSeekPositionProgress {
            generation: seek_commit.generation,
            target_position: seek_commit.target_position.as_duration(),
            committed_position,
            public_accepted_at: seek_commit.public_accepted_at,
            receipt_accepted_at: seek_commit.started_at,
            committed_at,
        });
    }

    /// Публикует evidence ровно на первом строго положительном post-commit delta.
    pub(crate) fn observe_position_progress(
        &mut self,
        observed_position: Duration,
        observed_at: Instant,
    ) -> Option<SeekPositionProgressEvidence> {
        let pending_progress = self.pending_position_progress?;
        if observed_position <= pending_progress.committed_position {
            return None;
        }

        self.pending_position_progress = None;
        Some(SeekPositionProgressEvidence {
            generation: pending_progress.generation,
            target_position: pending_progress.target_position,
            committed_position: pending_progress.committed_position,
            observed_position,
            public_elapsed: observed_at
                .saturating_duration_since(pending_progress.public_accepted_at),
            receipt_elapsed: observed_at
                .saturating_duration_since(pending_progress.receipt_accepted_at),
            commit_elapsed: observed_at.saturating_duration_since(pending_progress.committed_at),
        })
    }
}

#[cfg(test)]
mod tests {
    use media_core::MediaTime;

    use super::*;
    use crate::SeekMode;
    use crate::seek_state::{PlaybackResumeIntent, SeekTargetRetention};

    /// Создаёт детерминированный commit для чистых accounting tests.
    fn seek_commit(started_at: Instant) -> SeekCommitState {
        SeekCommitState {
            generation: 7,
            seek_mode: SeekMode::Accurate,
            target_position: MediaTime::from_secs(8),
            actual_position: MediaTime::from_secs(5),
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
            started_at,
            public_accepted_at: started_at,
            resume_intent: PlaybackResumeIntent::Play,
            target_retention: SeekTargetRetention::ExactPublicRange,
        }
    }

    #[test]
    fn pre_target_counter_is_zero_only_without_presented_violation() {
        let mut telemetry = SeekAcceptanceTelemetry::default();
        telemetry.begin_seek(seek_commit(Instant::now()));

        let zero_evidence = telemetry
            .record_first_target_frame_presented(7, Duration::from_secs(8))
            .expect("первый target frame должен дать evidence");
        assert_eq!(zero_evidence.presented_pre_target_frames(), 0);

        telemetry.begin_seek(seek_commit(Instant::now()));
        telemetry.record_presented_pre_target_frame(7, Duration::from_secs(6));
        let violation_evidence = telemetry
            .record_first_target_frame_presented(7, Duration::from_secs(8))
            .expect("target после нарушения всё равно должен сохранить counter");
        assert_eq!(violation_evidence.presented_pre_target_frames(), 1);
        assert_eq!(telemetry.commit_presentation_evidence(7), Some(1));
    }

    #[test]
    fn progress_is_one_shot_and_requires_positive_delta() {
        let started_at = Instant::now();
        let committed_at = started_at + Duration::from_millis(100);
        let mut telemetry = SeekAcceptanceTelemetry::default();
        telemetry.arm_position_progress(
            seek_commit(started_at),
            Duration::from_secs(8),
            committed_at,
        );

        assert_eq!(
            telemetry.observe_position_progress(Duration::from_secs(8), committed_at),
            None
        );
        let progress = telemetry
            .observe_position_progress(
                Duration::from_millis(8_020),
                committed_at + Duration::from_millis(20),
            )
            .expect("положительный delta должен дать один proof");
        assert_eq!(progress.generation(), 7);
        assert_eq!(progress.observed_position(), Duration::from_millis(8_020));
        assert_eq!(progress.commit_elapsed(), Duration::from_millis(20));
        assert_eq!(
            telemetry.observe_position_progress(
                Duration::from_millis(8_040),
                committed_at + Duration::from_millis(40),
            ),
            None
        );
    }

    #[test]
    fn progress_keeps_public_and_receipt_origins_distinct() {
        let public_accepted_at = Instant::now();
        let receipt_accepted_at = public_accepted_at + Duration::from_millis(1_500);
        let committed_at = receipt_accepted_at + Duration::from_millis(100);
        let observed_at = committed_at + Duration::from_millis(20);
        let mut commit = seek_commit(receipt_accepted_at);
        commit.public_accepted_at = public_accepted_at;
        let mut telemetry = SeekAcceptanceTelemetry::default();
        telemetry.arm_position_progress(commit, Duration::from_secs(8), committed_at);

        let progress = telemetry
            .observe_position_progress(Duration::from_millis(8_020), observed_at)
            .expect("положительный delta должен сохранить оба monotonic origin-а");

        assert_eq!(progress.public_elapsed(), Duration::from_millis(1_620));
        assert_eq!(progress.receipt_elapsed(), Duration::from_millis(120));
        assert_eq!(progress.commit_elapsed(), Duration::from_millis(20));
    }
}
