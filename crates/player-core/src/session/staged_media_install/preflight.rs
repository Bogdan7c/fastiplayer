//! Bounded continuation staged media preflight-а.
//!
//! Модуль владеет exact request/generation fence, единственным registry slot-ом
//! и deadline-driven preflight progression. Admission, detached backend commit
//! и terminal install protocol остаются у родительского модуля.

use std::time::{Duration, Instant};

use crate::{
    MediaInstallFailure, MediaInstallFailureStage, MediaInstallRequestId,
    MediaInstallVideoBackendConstraint, MediaInstanceId, PlayerError, PlayerErrorKind,
    PreparedMedia,
};

use super::super::staged_video_preflight::{
    StagedVideoPlanner, StagedVideoPlanningMode, StagedVideoPlanningOutcome,
};
use super::{
    PlayerSession, PreparedStagedMedia, StagedAudioTrackPlan, StagedMediaInstall,
    StagedMediaPreparation, prepare_detached_video_backend,
};

/// Typed generation request-owned staged preflight-а.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StagedPreflightGeneration(pub(super) u64);

/// Fence до появления будущего `MediaInstanceId`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct StagedPreflightFence {
    /// Exact install request не позволяет продолжить superseded candidate.
    pub(super) request_id: MediaInstallRequestId,

    /// Owner generation защищает от повторного использования registry slot-а.
    pub(super) generation: StagedPreflightGeneration,
}

/// Player-owned continuation staged video preflight-а.
pub(super) struct PendingStagedPreflight {
    /// Request + staged-generation fence continuation-а.
    pub(super) fence: StagedPreflightFence,

    /// Detached media остаётся у player owner-а до terminal outcome.
    pub(super) prepared_media: PreparedMedia,

    /// Audio plan вычисляется один раз и не повторяет fallible работу после retry.
    pub(super) audio_plan: Option<StagedAudioTrackPlan>,

    /// Exact/compatibility policy исходного ingress-а.
    pub(super) video_planning_mode: StagedVideoPlanningMode,

    /// Packet reader, budgets и current-track progress сохраняются между wakeup-ами.
    pub(super) video_planner: StagedVideoPlanner,

    /// Earliest retry текущего temporary readiness event-а.
    pub(super) retry_deadline: Option<Instant>,

    /// Независимый terminal wall-clock deadline всего preflight-а.
    pub(super) timeout_deadline: Instant,
}

/// Bounded owner slot: максимум одна staged transaction и один terminal tombstone.
#[derive(Default)]
pub(in crate::session) struct StagedMediaInstallRegistry {
    /// Текущий cancellable request до accepted authorization.
    pub(super) active: Option<StagedMediaInstall>,

    /// Последний terminal request нужен для typed duplicate rejection после cleanup.
    pub(super) last_terminal_request_id: Option<MediaInstallRequestId>,

    /// Монотонная owner generation новых preflight continuation-ов.
    pub(super) next_preflight_generation: u64,
}

impl PlayerSession {
    /// Продолжает один exact staged preflight owner-step без блокировки worker loop-а.
    pub(crate) fn service_pending_staged_preflight(&mut self, now: Instant) {
        let Some(mut staged_install) = self.staged_media_install.active.take() else {
            return;
        };
        let preparation = std::mem::replace(
            &mut staged_install.preparation,
            StagedMediaPreparation::Ready(None),
        );
        let StagedMediaPreparation::Pending(mut pending) = preparation else {
            staged_install.preparation = preparation;
            self.staged_media_install.active = Some(staged_install);
            return;
        };
        let current_generation =
            StagedPreflightGeneration(self.staged_media_install.next_preflight_generation);
        if pending.fence.request_id != staged_install.request_id
            || pending.fence.generation != current_generation
        {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::VideoStreamConfiguration,
                    PlayerError::new(
                        PlayerErrorKind::RuntimeError,
                        "Staged preflight потерял exact request fence",
                    ),
                ),
            );
            return;
        }
        if now >= pending.timeout_deadline {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::VideoPreflightTimeout,
                    PlayerError::new(
                        PlayerErrorKind::DemuxError,
                        "Staged video preflight превысил bounded wall-clock deadline",
                    ),
                ),
            );
            return;
        }
        if pending
            .retry_deadline
            .is_some_and(|retry_deadline| now < retry_deadline)
        {
            staged_install.preparation = StagedMediaPreparation::Pending(pending);
            self.staged_media_install.active = Some(staged_install);
            return;
        }

        pending.retry_deadline = None;
        let backend_constraint = staged_install.video_resource_port.as_ref().map_or(
            MediaInstallVideoBackendConstraint::AnyPlayable,
            |video_resource_port| video_resource_port.backend_constraint().clone(),
        );
        let video_plan = match self.resume_staged_video_track_plan(
            &mut pending.prepared_media,
            pending.video_planning_mode,
            &backend_constraint,
            &mut pending.video_planner,
        ) {
            Ok(StagedVideoPlanningOutcome::Ready(video_plan)) => video_plan,
            Ok(StagedVideoPlanningOutcome::Pending(hint)) => {
                pending.retry_deadline = Some(now.checked_add(hint.retry_after()).unwrap_or(now));
                staged_install.preparation = StagedMediaPreparation::Pending(pending);
                self.staged_media_install.active = Some(staged_install);
                return;
            }
            Err(failure) => {
                self.finish_staged_preflight_failure(
                    staged_install,
                    MediaInstallFailure::new(
                        MediaInstallFailureStage::VideoStreamConfiguration,
                        failure,
                    ),
                );
                return;
            }
        };
        if let Err(error) = pending.prepared_media.prepare_playback_window() {
            self.finish_staged_preflight_failure(
                staged_install,
                MediaInstallFailure::new(
                    MediaInstallFailureStage::PlaybackWindowPreparation,
                    PlayerError::new(
                        PlayerErrorKind::SeekUnavailable,
                        format!("Не удалось подготовить playback window: {error}"),
                    ),
                ),
            );
            return;
        }
        let mut prepared_media_envelope = pending.prepared_media;
        let demux_seek_runtime =
            super::super::prepared_demux_seek::PreparedDemuxSeekRuntime::detached(
                prepared_media_envelope.take_demux_seek_mode(),
            );
        let prepared_media = PreparedStagedMedia {
            prepared_media: prepared_media_envelope,
            audio_plan: pending.audio_plan,
            video_plan,
            media_instance_id: MediaInstanceId::new_unique(),
            demux_seek_runtime,
        };
        let started_video_backend = match staged_install.video_resource_port.as_mut() {
            Some(video_resource_port) => match prepare_detached_video_backend(
                staged_install.request_id,
                &prepared_media,
                video_resource_port,
            ) {
                Ok(started_video_backend) => started_video_backend,
                Err(failure) => {
                    self.finish_staged_preflight_failure(staged_install, failure);
                    return;
                }
            },
            None => None,
        };

        if staged_install.position_preparation.is_required() {
            staged_install
                .protocol
                .mark_ready_for_position_preparation();
        } else {
            staged_install.protocol.mark_ready_to_commit();
        }
        staged_install.preparation = StagedMediaPreparation::Ready(Some(prepared_media));
        staged_install.started_video_backend = started_video_backend;
        self.staged_media_install.active = Some(staged_install);
    }

    /// Terminal-resolve-ит pre-Ready failure ровно один раз.
    fn finish_staged_preflight_failure(
        &mut self,
        mut staged_install: StagedMediaInstall,
        failure: MediaInstallFailure,
    ) {
        staged_install.protocol.complete_failed(failure);
        self.playback_intent_control
            .forget_staged_request(staged_install.request_id);
        self.staged_media_install.last_terminal_request_id = Some(staged_install.request_id);
    }

    /// Возвращает ближайший retry/timeout deadline active staged continuation-а.
    #[must_use]
    pub(crate) fn staged_preflight_wakeup_delay(&self, now: Instant) -> Option<Duration> {
        let preflight_delay = self
            .staged_media_install
            .active
            .as_ref()
            .and_then(|staged| {
                let StagedMediaPreparation::Pending(pending) = &staged.preparation else {
                    return None;
                };
                let retry_deadline = pending.retry_deadline.unwrap_or(now);
                let nearest_deadline = retry_deadline.min(pending.timeout_deadline);
                Some(nearest_deadline.saturating_duration_since(now))
            });
        match (preflight_delay, self.staged_position_wakeup_delay()) {
            (Some(left), Some(right)) => Some(left.min(right)),
            (Some(delay), None) | (None, Some(delay)) => Some(delay),
            (None, None) => None,
        }
    }
}
