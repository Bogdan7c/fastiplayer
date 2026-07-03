use std::time::{Duration, Instant};

use frame_server_core::{
    AudioResumeTimedOutOutcome, BackendRevision, CancelScrubReason, LiveScrubDiagnostics,
    PlaybackGeneration, ScrubDriverOutcome, ScrubEvent, ScrubExactnessPolicy, ScrubGeneration,
    ScrubGenerationToken, ScrubRequestKind, ScrubTarget, ScrubTargetContext, ScrubTrackSelection,
    SourceRevision,
};
use media_core::{MediaTime, TimelinePreviewState};

use super::PlayerSession;
use super::audio_runtime::SeekAudioGateStatus;
use super::scrub_driver::{AudioResumeTimingInput, derive_audio_resume_timeout_budget};
use crate::seek_state::{
    PlaybackResumeIntent, SeekCommitState, SeekLandingExecution, SeekLandingGenerationStartError,
};
use crate::{PlaybackState, PlayerError, PlayerErrorKind, PlayerResult, SeekMode};

/// Source revision пока остаётся untracked: cold SeekLanding владеет реальным stale guard-ом.
pub(super) const SEEK_LANDING_SOURCE_REVISION_UNTRACKED: u64 = 0;

/// Backend revision пока остаётся untracked, потому отдельная подготовленная ветка больше не ведётся.
pub(super) const SEEK_LANDING_BACKEND_REVISION_UNTRACKED: u64 = 0;

/// Первый nested scrub generation совпадает с `ScrubStateMachine` start semantics.
pub(super) const SEEK_LANDING_FIRST_SCRUB_GENERATION: u64 = 1;

/// Prepared route больше не держит frame ownership.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedSeekLandingReleaseOutcome {
    /// Active promoted resource отсутствует.
    NoPromotedFrame,
}

/// Runtime state SeekLanding без отдельного prepared working set-а.
#[derive(Debug, Default, Clone)]
pub(super) struct PreparedSeekLandingRuntime;

impl PreparedSeekLandingRuntime {
    /// Prepared ownership больше не существует, поэтому очистка является no-op.
    pub(super) fn clear_promoted_seek_ownership(&mut self) {}

    /// Prepared ownership больше не существует, поэтому release всегда сообщает отсутствие frame-а.
    pub(super) fn release_promoted_seek_ownership(
        &mut self,
        _reason: CancelScrubReason,
    ) -> PreparedSeekLandingReleaseOutcome {
        PreparedSeekLandingReleaseOutcome::NoPromotedFrame
    }
}

impl PlayerSession {
    /// Активирует SeekLanding generation для cold/live scrub driver-а.
    pub(super) fn activate_seek_landing_scrub_generation(
        &mut self,
        generation: ScrubGenerationToken,
        execution: SeekLandingExecution,
        decode_seek_generation: Option<u64>,
    ) -> Result<(), SeekLandingGenerationStartError> {
        let expected_generation = self
            .seek_runtime
            .pending_seek_landing_generation()
            .unwrap_or_else(|| {
                let playback_generation = self
                    .seek_runtime
                    .seek_landing_playback_generation()
                    .unwrap_or(generation.playback_generation);
                ScrubGenerationToken::new(playback_generation, generation.scrub_generation)
            });

        if let Some(stale_reason) = generation.stale_reason_against(expected_generation) {
            return Err(SeekLandingGenerationStartError::Stale(stale_reason));
        }

        self.seek_runtime.activate_seek_landing_generation(
            generation,
            execution,
            decode_seek_generation,
        );
        Ok(())
    }

    /// Сбрасывает pending packets/frames после принятия нового SeekLanding generation.
    pub(super) fn clear_pending_queues_for_seek_landing(&mut self) {
        let has_video = self.pipeline.has_selected_video_track();
        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        self.clear_queued_video_frames();
    }

    /// Prepared route больше не активируется, поэтому commit match всегда false.
    pub(super) fn active_prepared_seek_landing_matches_commit(
        &self,
        _seek_commit: SeekCommitState,
    ) -> bool {
        false
    }

    /// Prepared route больше не закрывает video gate; cold SeekLanding остаётся владельцем readiness.
    pub(super) fn prepared_seek_video_runway_commit_ready(
        &self,
        _seek_commit: SeekCommitState,
    ) -> bool {
        false
    }

    /// Восстанавливает active SeekLanding context из commit-а и owner state.
    pub(super) fn active_seek_landing_context(
        &self,
        seek_commit: SeekCommitState,
    ) -> Option<ScrubTargetContext> {
        let landing = self
            .seek_runtime
            .active_seek_landing_for_commit_generation(seek_commit.generation)?;
        let video_track_id = self.pipeline.selected_video_track_id()?;
        let target = ScrubTarget::new(
            seek_commit.target_position,
            self.seek_landing_target_pts(video_track_id, seek_commit.target_position),
        );
        let track_selection = match self.pipeline.selected_audio_track_id() {
            Some(audio_track_id) => ScrubTrackSelection::with_audio(video_track_id, audio_track_id),
            None => ScrubTrackSelection::video_only(video_track_id),
        };

        Some(prepared_seek_landing_context(
            track_selection,
            target,
            landing.generation(),
            landing.route().request_kind(),
        ))
    }

    /// Отпускает prepared ownership. Сейчас отдельного ownership state нет, поэтому это no-op.
    pub(super) fn release_prepared_seek_landing_for_cancel(
        &mut self,
        cancel_reason: CancelScrubReason,
        _context: Option<ScrubTargetContext>,
    ) -> PreparedSeekLandingReleaseOutcome {
        self.prepared_seek_landing
            .release_promoted_seek_ownership(cancel_reason)
    }

    /// Очищает prepared route state. Сейчас отдельного route state нет.
    pub(super) fn clear_prepared_seek_landing_with_diagnostics(&mut self) {
        self.prepared_seek_landing.clear_promoted_seek_ownership();
    }

    /// Prepared route больше не стартует, но timeout path остаётся fail-closed.
    pub(super) fn fail_prepared_seek_landing_audio_resume_on_timeout(
        &mut self,
        seek_commit: SeekCommitState,
        audio_gate_status: SeekAudioGateStatus,
        timed_out_at: Instant,
    ) {
        let Some(context) = self.active_seek_landing_context(seek_commit) else {
            self.fail_final_seek_commit_on_timeout(
                seek_commit,
                audio_gate_status
                    .blocker()
                    .unwrap_or(crate::SeekProgressBlocker::Unknown),
            );
            return;
        };
        let budget = self.prepared_audio_resume_budget(
            timed_out_at.saturating_duration_since(seek_commit.started_at),
        );
        let live_scrub_diagnostics = self.seek_runtime.active_seek_landing_live_diagnostics();
        self.push_scrub_event_with_live_diagnostics(
            ScrubEvent::from_driver_outcome(ScrubDriverOutcome::AudioResumeTimedOut(
                AudioResumeTimedOutOutcome {
                    context,
                    budget: budget.metadata,
                },
            )),
            live_scrub_diagnostics,
        );

        self.seek_runtime.clear_active_commit();
        self.clear_prepared_seek_landing_with_diagnostics();
        self.seek_runtime.clear_trace();
        self.seek_runtime.clear_seek_landing();
        self.seek_runtime.clear_eof_fallback_video_position();
        self.clear_seek_preroll_fallback_frame();
        self.snapshot.timeline.target_position = None;
        self.snapshot.timeline.seeking = false;
        self.snapshot.timeline.scrubbing = false;
        self.snapshot.timeline.preview_state = TimelinePreviewState::Failed;
        self.snapshot.timeline.stale_frame = self.pipeline.has_present_video_frame()
            && !self.present_frame_covers_target(seek_commit.target_position.as_duration());
        let restored_position = self
            .pipeline
            .present_video_frame()
            .map_or(self.snapshot.current_position, |frame| frame.pts);
        self.pipeline.set_media_clock_base(restored_position);
        self.pipeline.clear_monotonic_media_clock();
        self.pause_audio_output_for_seek();
        match seek_commit.resume_intent {
            PlaybackResumeIntent::Pause => self.set_playback_state(PlaybackState::Paused),
            PlaybackResumeIntent::Play => self.set_playback_state(PlaybackState::Playing),
        }

        let fallback_suffix = if matches!(
            budget.metadata.source,
            frame_server_core::AudioResumeBudgetSource::TimingUnknownFallback
        ) {
            "; audio_timing_unknown_fallback=true"
        } else {
            ""
        };
        let error = PlayerError::new(
            PlayerErrorKind::SeekTimeout,
            format!(
                "Prepared seek audio resume timeout: target={} ms, blocker={}, budget={} ms{}",
                seek_commit.target_position.as_duration().as_millis(),
                audio_gate_status
                    .blocker()
                    .map_or("unknown", crate::SeekProgressBlocker::metric_name),
                budget.metadata.budget.as_millis(),
                fallback_suffix
            ),
        );
        self.record_recoverable_error(error);
    }

    /// Отдельный prepared route удалён: caller должен продолжить cold SeekLanding path.
    #[allow(
        clippy::too_many_arguments,
        reason = "Signature intentionally preserves the existing seek_transaction boundary."
    )]
    pub(super) fn confirm_prepared_seek_landing_unavailable(
        &mut self,
        _target_position: MediaTime,
        _seek_mode: SeekMode,
        _resume_intent: PlaybackResumeIntent,
        _generation: ScrubGenerationToken,
        _track_selection: ScrubTrackSelection,
        _target: ScrubTarget,
        _request_kind: ScrubRequestKind,
        _commit_before_release_allowed: bool,
        _live_scrub_diagnostics: Option<LiveScrubDiagnostics>,
    ) -> PlayerResult<()> {
        Ok(())
    }

    /// Начинает новый decoder seek generation для cold SeekLanding route-а.
    pub(super) fn begin_cold_seek_landing_decoder_generation(
        &mut self,
    ) -> Result<u64, SeekLandingGenerationStartError> {
        let expected_decoder_generation = self
            .pipeline
            .seek_generation()
            .checked_add(1)
            .ok_or(SeekLandingGenerationStartError::GenerationOverflow)?;
        let new_decoder_generation = self.pipeline.begin_seek_generation();
        debug_assert_eq!(new_decoder_generation, expected_decoder_generation);
        if self.pipeline.has_selected_video_track() {
            self.record_video_decoder_bootstrap_started();
        }
        self.reset_audio_runtime_for_seek_landing(new_decoder_generation);

        Ok(new_decoder_generation)
    }

    /// Сбрасывает audio runtime для seek generation, не меняя video decoder epoch.
    pub(super) fn reset_audio_runtime_for_seek_landing(&mut self, generation: u64) {
        if let Some(Err(error)) = self.pipeline.reset_audio_decoder() {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio decoder reset failed during SeekLanding: {error}"),
            );
            self.record_recoverable_error(player_error);
        }
        if let Some(clear_result) = self.pipeline.clear_audio_output_for_seek(generation) {
            match clear_result {
                Ok(ack_generation) => {
                    self.pipeline.mark_audio_buffer_clear_ack(ack_generation);
                }
                Err(error) => {
                    let player_error = PlayerError::new(
                        PlayerErrorKind::AudioDeviceUnavailable,
                        format!("Audio buffer clear failed during SeekLanding: {error}"),
                    );
                    self.record_recoverable_error(player_error);
                }
            }
        } else {
            self.pipeline.mark_audio_buffer_clear_ack(generation);
            self.pipeline.reset_audio_clock();
        }
    }

    /// Считает audio resume budget для timeout diagnostics.
    fn prepared_audio_resume_budget(
        &self,
        elapsed: Duration,
    ) -> super::scrub_driver::PlayerAudioResumeBudget {
        let current_output_buffer = self.audio_buffer_level_ms().map(duration_from_millis_f64);

        derive_audio_resume_timeout_budget(
            AudioResumeTimingInput {
                current_output_buffer,
                callback_or_device_period: None,
            },
            elapsed,
        )
    }
}

/// Создаёт context, совпадающий с первым context нового one-shot SeekLanding driver-а.
#[must_use]
pub(super) fn prepared_seek_landing_context(
    track_selection: ScrubTrackSelection,
    target: ScrubTarget,
    generation: ScrubGenerationToken,
    request_kind: ScrubRequestKind,
) -> ScrubTargetContext {
    ScrubTargetContext::new(
        SourceRevision::new(SEEK_LANDING_SOURCE_REVISION_UNTRACKED),
        BackendRevision::new(SEEK_LANDING_BACKEND_REVISION_UNTRACKED),
        track_selection,
        target,
        ScrubExactnessPolicy::ExactFrame,
        request_kind,
        generation,
    )
}

/// Возвращает generation token для player-owned one-shot SeekLanding context-а.
#[must_use]
pub(super) fn seek_landing_generation_token(
    playback_generation: u64,
    scrub_generation: ScrubGeneration,
) -> ScrubGenerationToken {
    ScrubGenerationToken::new(
        PlaybackGeneration::new(playback_generation),
        scrub_generation,
    )
}

/// Переводит diagnostics buffer level в `Duration`, отсекая NaN/inf/negative значения.
fn duration_from_millis_f64(milliseconds: f64) -> Duration {
    if !milliseconds.is_finite() || milliseconds <= 0.0 {
        return Duration::ZERO;
    }

    Duration::from_secs_f64(milliseconds / 1_000.0)
}
