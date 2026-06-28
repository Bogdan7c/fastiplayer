use std::num::NonZeroUsize;
use std::time::{Duration, Instant};

use frame_server_core::{
    AudioResumePendingOutcome, BackendRevision, ExactFrameReadyOutcome, FrameExactnessPolicy,
    FrameServerConfig, PlaybackGeneration, PreparedOutcome, ScrubDriverOutcome, ScrubEvent,
    ScrubExactnessPolicy, ScrubFrameTiming, ScrubGeneration, ScrubGenerationToken,
    ScrubPreviewFrame, ScrubRequestKind, ScrubTarget, ScrubTargetContext, ScrubTrackSelection,
    SourceRevision, TimelineHoverFrameBucket, TimelineHoverPrepareFrameKey,
    TimelineHoverPrepareFrameLookupRequest, TimelineHoverPreparePromotionOutcome,
    TimelineHoverPrepareWorkingSet, TimelineHoverPromotedFrameSeekReuse,
    TimelineHoverPromotedPreparedFrame, TimelineHoverRecentSupersededBudget,
    ValidatedFrameServerConfig,
};
#[cfg(test)]
use frame_server_core::{TimelineHoverPreparedFrameEntry, TimelineHoverPreparedFrameTiming};
use media_core::{MediaTime, TrackTimestamp};
use video_present_core::{VideoFrameLease, VideoPresentFrameIdentity};

use super::PlayerSession;
use super::scrub_driver::{AudioResumeTimingInput, derive_audio_resume_timeout_budget};
use crate::seek_state::{
    PlaybackResumeIntent, SeekCommitState, SeekLandingExecution, SeekLandingGenerationStartError,
};
use crate::{PlayerError, PlayerErrorKind, PlayerResult, SeekMode};

/// S17A/S17B пока не имеют отдельного source revision provider-а внутри player-core.
/// Playback generation остаётся реальным stale guard-ом для decoded frames.
pub(super) const SEEK_LANDING_SOURCE_REVISION_UNTRACKED: u64 = 0;

/// Backend revision будет заменён реальным owner counter-ом, когда hover/prepared
/// branch integration начнёт валидировать backend lineage.
pub(super) const SEEK_LANDING_BACKEND_REVISION_UNTRACKED: u64 = 0;

/// `PlayerScrubTransactionDriver::new(..., ScrubGeneration::new(0))` создаёт
/// первый `SeekLanding` context с nested scrub generation `1`.
const SEEK_LANDING_PREPARED_SCRUB_GENERATION: u64 = 1;

/// Internal command для worker-side scrub override bridge.
pub(crate) enum PreparedSeekLandingOverrideHandoff {
    /// Опубликовать clone seek-owned lease-а в отдельный scrub override slot.
    Publish(VideoFrameLease),

    /// Очистить slot, когда S17 transaction закончилась или была сброшена.
    Clear,
}

/// Минимальный player-core token, который доказывает готовность prepared branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct PreparedSeekBranchToken {
    continuation: PreparedBranchContinuation,
    runway: PreparedBranchRunway,
}

impl PreparedSeekBranchToken {
    /// Создаёт token только для полностью готового continuation/runway.
    #[cfg(test)]
    pub(crate) const fn resume_ready_for_tests() -> Self {
        Self {
            continuation: PreparedBranchContinuation::Ready,
            runway: PreparedBranchRunway::Ready,
        }
    }

    /// Создаёт token с branch continuation, но без готовой resume runway.
    #[cfg(test)]
    pub(crate) const fn continuation_without_runway_for_tests() -> Self {
        Self {
            continuation: PreparedBranchContinuation::Ready,
            runway: PreparedBranchRunway::Pending,
        }
    }

    /// Fail-closed validation: token presence alone never proves resume readiness.
    fn validate(self) -> PreparedSeekBranchValidation {
        match (self.continuation, self.runway) {
            (PreparedBranchContinuation::Ready, PreparedBranchRunway::Ready) => {
                PreparedSeekBranchValidation::ResumeReady
            }
            (PreparedBranchContinuation::Missing, _) => {
                PreparedSeekBranchValidation::ResumePending {
                    reason: PreparedSeekBranchResumePendingReason::ContinuationMissing,
                }
            }
            (PreparedBranchContinuation::Ready, PreparedBranchRunway::Pending) => {
                PreparedSeekBranchValidation::ResumePending {
                    reason: PreparedSeekBranchResumePendingReason::RunwayPending,
                }
            }
        }
    }
}

/// Branch continuation readiness отдельно от resume runway readiness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "S17B token is test-fed until the real hover executor starts constructing branch readiness."
)]
enum PreparedBranchContinuation {
    Ready,
    Missing,
}

/// Минимальная S17B readiness runway без S17C audio timeout policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(
    dead_code,
    reason = "S17B token is test-fed until the real hover executor starts constructing branch readiness."
)]
enum PreparedBranchRunway {
    Ready,
    Pending,
}

/// Результат player-side branch validation после neutral promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PreparedSeekBranchValidation {
    ResumeReady,
    ResumePending {
        reason: PreparedSeekBranchResumePendingReason,
    },
}

/// Почему promoted branch нельзя считать resume-ready.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedSeekBranchResumePendingReason {
    FrameOnly,
    ContinuationMissing,
    RunwayPending,
}

/// Вид prepared promotion-а, который player-core реально принял во владение.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PreparedSeekLandingPromotionKind {
    /// Branch token доказал continuation + runway, но S17B всё равно не делает instant commit.
    ResumeReadyBranch,

    /// Frame подходит как exact visual override, а resume остаётся pending.
    VisualOverrideResumePending {
        reason: PreparedSeekBranchResumePendingReason,
    },
}

/// Seek-owned promoted resource после удаления из hover working set-а.
struct PreparedSeekLandingPromotion {
    promoted_frame: TimelineHoverPromotedPreparedFrame<PreparedSeekBranchToken>,
    #[allow(
        dead_code,
        reason = "Focused S17B tests inspect the accepted promotion kind; production only needs ownership."
    )]
    kind: PreparedSeekLandingPromotionKind,
}

impl PreparedSeekLandingPromotion {
    fn lease(&self) -> &VideoFrameLease {
        self.promoted_frame.lease()
    }
}

/// Runtime bridge между neutral prepared working set и S17 seek transaction.
pub(super) struct PreparedSeekLandingRuntime {
    working_set: TimelineHoverPrepareWorkingSet<PreparedSeekBranchToken>,
    promoted: Option<PreparedSeekLandingPromotion>,
    pending_override_handoff: Option<PreparedSeekLandingOverrideHandoff>,
}

impl PreparedSeekLandingRuntime {
    /// Создаёт runtime из validated frame-server config без отдельного hardcode capacity.
    #[must_use]
    pub(super) fn from_config(config: ValidatedFrameServerConfig) -> Self {
        let primary_capacity = NonZeroUsize::new(config.hover_prepare_window_slots() as usize)
            .expect("validated hover prepare slots must be non-zero");
        let recent_budget = TimelineHoverRecentSupersededBudget::from_validated_config(config);

        Self {
            working_set: TimelineHoverPrepareWorkingSet::with_capacity_and_recent_superseded(
                primary_capacity,
                recent_budget,
            ),
            promoted: None,
            pending_override_handoff: None,
        }
    }

    /// Пробует promoted prepared frame для конкретного guarded seek target-а.
    fn promote_for_seek(
        &mut self,
        request: TimelineHoverPrepareFrameLookupRequest,
    ) -> PreparedSeekLandingPromotionAttempt {
        let promotion = self.working_set.promote_prepared_frame(request);

        match promotion {
            TimelineHoverPreparePromotionOutcome::PromotedResumeReadyBranch(promoted_frame) => {
                PreparedSeekLandingPromotionAttempt::Promoted(
                    self.store_promoted_frame(promoted_frame),
                )
            }
            TimelineHoverPreparePromotionOutcome::PromotedVisualOverrideResumePending(
                promoted_frame,
            ) => PreparedSeekLandingPromotionAttempt::Promoted(
                self.store_promoted_frame(promoted_frame),
            ),
            TimelineHoverPreparePromotionOutcome::Miss(_)
            | TimelineHoverPreparePromotionOutcome::TimingRejected(_) => {
                PreparedSeekLandingPromotionAttempt::Unavailable
            }
        }
    }

    /// Переносит promoted resource в seek ownership и ставит override handoff.
    fn store_promoted_frame(
        &mut self,
        promoted_frame: TimelineHoverPromotedPreparedFrame<PreparedSeekBranchToken>,
    ) -> PreparedSeekLandingPromotionKind {
        let kind = match promoted_frame.seek_reuse() {
            TimelineHoverPromotedFrameSeekReuse::ResumeReadyBranch { branch_token } => {
                match branch_token.validate() {
                    PreparedSeekBranchValidation::ResumeReady => {
                        PreparedSeekLandingPromotionKind::ResumeReadyBranch
                    }
                    PreparedSeekBranchValidation::ResumePending { reason } => {
                        PreparedSeekLandingPromotionKind::VisualOverrideResumePending { reason }
                    }
                }
            }
            TimelineHoverPromotedFrameSeekReuse::VisualOverrideResumePending => {
                PreparedSeekLandingPromotionKind::VisualOverrideResumePending {
                    reason: PreparedSeekBranchResumePendingReason::FrameOnly,
                }
            }
        };
        let override_lease = promoted_frame.lease().clone();

        self.promoted = Some(PreparedSeekLandingPromotion {
            promoted_frame,
            kind,
        });
        self.pending_override_handoff =
            Some(PreparedSeekLandingOverrideHandoff::Publish(override_lease));

        kind
    }

    /// Очищает seek-owned promotion; hover-owned entries не трогает.
    pub(super) fn clear_promoted_seek_ownership(&mut self) {
        if self.promoted.take().is_some() {
            self.pending_override_handoff = Some(PreparedSeekLandingOverrideHandoff::Clear);
        }
    }

    /// Забирает одноразовую команду для worker-to-app scrub override bridge.
    pub(super) fn take_override_handoff(&mut self) -> Option<PreparedSeekLandingOverrideHandoff> {
        self.pending_override_handoff.take()
    }

    #[cfg(test)]
    pub(crate) fn insert_prepared_frame_for_tests(
        &mut self,
        key: TimelineHoverPrepareFrameKey,
        lease: VideoFrameLease,
        actual_pts: TrackTimestamp,
        branch_token: Option<PreparedSeekBranchToken>,
    ) {
        let entry = TimelineHoverPreparedFrameEntry::new(
            lease,
            TimelineHoverPreparedFrameTiming::new(actual_pts),
        );
        let entry = match branch_token {
            Some(token) => entry.with_branch_token(token),
            None => entry,
        };

        self.working_set.insert_prepared_frame(key, entry);
    }

    #[cfg(test)]
    pub(crate) fn working_set_len_for_tests(&self) -> usize {
        self.working_set.len()
    }

    #[cfg(test)]
    pub(crate) fn active_promotion_kind_for_tests(
        &self,
    ) -> Option<PreparedSeekLandingPromotionKind> {
        self.promoted.as_ref().map(|promotion| promotion.kind)
    }
}

impl Default for PreparedSeekLandingRuntime {
    fn default() -> Self {
        let config = FrameServerConfig::default()
            .validate()
            .expect("default frame-server config must validate");
        Self::from_config(config)
    }
}

/// Outcome попытки S17B prepared route до cold decode fallback-а.
enum PreparedSeekLandingPromotionAttempt {
    Promoted(PreparedSeekLandingPromotionKind),
    Unavailable,
}

/// Outcome player-side route decision для caller-а в `seek_transaction`.
pub(super) enum PreparedSeekLandingStart {
    Started,
    Unavailable,
}

impl PlayerSession {
    /// Начинает playback generation для SeekLanding без смешивания с decode route.
    pub(super) fn begin_seek_landing_playback_generation(
        &mut self,
        generation: ScrubGenerationToken,
        execution: SeekLandingExecution,
    ) -> Result<(), SeekLandingGenerationStartError> {
        let pending_seek_landing = self.seek_runtime.pending_seek_landing_request_active();
        let expected_playback_generation = if pending_seek_landing {
            self.pipeline
                .seek_generation()
                .checked_add(1)
                .ok_or(SeekLandingGenerationStartError::GenerationOverflow)?
        } else {
            self.pipeline.seek_generation()
        };
        let current_generation = ScrubGenerationToken::new(
            PlaybackGeneration::new(expected_playback_generation),
            generation.scrub_generation,
        );
        if let Some(stale_reason) = generation.stale_reason_against(current_generation) {
            return Err(SeekLandingGenerationStartError::Stale(stale_reason));
        }

        if pending_seek_landing {
            let new_playback_generation = self.begin_pipeline_seek_generation();
            debug_assert_eq!(new_playback_generation, expected_playback_generation);
        }

        self.seek_runtime
            .activate_seek_landing_generation(generation, execution);
        Ok(())
    }

    /// Сбрасывает pending packets/frames после принятия нового SeekLanding generation.
    pub(super) fn clear_pending_queues_for_seek_landing(&mut self) {
        let has_video = self.pipeline.has_selected_video_track();
        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        self.clear_queued_video_frames();
    }

    /// Пробует prepared frame до cold reused-decoder route.
    pub(super) fn start_prepared_seek_landing_if_available(
        &mut self,
        target_position: MediaTime,
        seek_mode: SeekMode,
        resume_intent: PlaybackResumeIntent,
        generation: u64,
        track_selection: ScrubTrackSelection,
        target: ScrubTarget,
    ) -> PlayerResult<PreparedSeekLandingStart> {
        if seek_mode != SeekMode::Accurate {
            return Ok(PreparedSeekLandingStart::Unavailable);
        }

        let context = prepared_seek_landing_context(track_selection, target, generation);
        let lookup_request = prepared_seek_landing_lookup_request(context);
        let promotion = self.prepared_seek_landing.promote_for_seek(lookup_request);

        let PreparedSeekLandingPromotionAttempt::Promoted(_promotion_kind) = promotion else {
            return Ok(PreparedSeekLandingStart::Unavailable);
        };

        if let Err(error) = self.clear_active_seek_decoder_output_floor("prepared seek landing") {
            self.prepared_seek_landing.clear_promoted_seek_ownership();
            self.record_recoverable_error(error.clone());
            return Err(error);
        }

        if let Err(error) = self.begin_seek_landing_playback_generation(
            context.generation(),
            SeekLandingExecution::PreparedVisualOverride,
        ) {
            self.prepared_seek_landing.clear_promoted_seek_ownership();
            return Err(player_error_from_seek_landing_generation_error(error));
        }
        self.clear_pending_queues_for_seek_landing();

        let seek_commit = SeekCommitState {
            generation,
            seek_mode,
            target_position,
            actual_position: target_position,
            started_at: Instant::now(),
            resume_intent,
        };
        self.reanchor_clocks_after_seek_accept(seek_commit);
        self.seek_runtime.set_active_commit(seek_commit);

        self.pending_scrub_events
            .push(ScrubEvent::from_driver_outcome(
                ScrubDriverOutcome::Prepared(PreparedOutcome { context }),
            ));
        let preview_frame = self.prepared_seek_landing_preview_frame(context);
        self.pending_scrub_events
            .push(ScrubEvent::from_driver_outcome(
                ScrubDriverOutcome::ExactFrameReady(ExactFrameReadyOutcome {
                    context,
                    frame: preview_frame,
                }),
            ));
        self.pending_scrub_events
            .push(ScrubEvent::from_driver_outcome(
                ScrubDriverOutcome::AudioResumePending(AudioResumePendingOutcome {
                    context,
                    budget: prepared_audio_resume_budget(),
                }),
            ));

        Ok(PreparedSeekLandingStart::Started)
    }

    /// Начинает новый playback seek generation и сбрасывает audio state.
    fn begin_pipeline_seek_generation(&mut self) -> u64 {
        let new_playback_generation = self.pipeline.begin_seek_generation();
        if self.pipeline.has_selected_video_track() {
            self.record_video_decoder_bootstrap_started();
        }
        if let Some(Err(error)) = self.pipeline.reset_audio_decoder() {
            let player_error = PlayerError::new(
                PlayerErrorKind::RuntimeError,
                format!("Audio decoder reset failed during SeekLanding: {error}"),
            );
            self.record_recoverable_error(player_error);
        }
        if let Some(clear_result) = self
            .pipeline
            .clear_audio_output_for_seek(new_playback_generation)
        {
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
            self.pipeline
                .mark_audio_buffer_clear_ack(new_playback_generation);
            self.pipeline.reset_audio_clock();
        }

        new_playback_generation
    }

    /// Собирает preview frame из seek-owned promoted lease-а.
    fn prepared_seek_landing_preview_frame(
        &self,
        context: ScrubTargetContext,
    ) -> ScrubPreviewFrame {
        let promotion = self
            .prepared_seek_landing
            .promoted
            .as_ref()
            .expect("prepared promotion must be stored before preview event");
        let lease = promotion.lease();
        let actual_pts = promotion.promoted_frame.timing().actual_pts();
        let timing = ScrubFrameTiming::new(actual_pts.to_media_time(), actual_pts);

        ScrubPreviewFrame {
            generation: context.generation(),
            timing,
            frame_identity: VideoPresentFrameIdentity::from_decoded_frame(
                lease.render_generation(),
                lease.decoded_frame(),
            ),
            resource: lease.resource_descriptor(),
        }
    }

    /// Забирает pending prepared override handoff для worker runtime.
    pub(crate) fn take_prepared_seek_landing_override_handoff(
        &mut self,
    ) -> Option<PreparedSeekLandingOverrideHandoff> {
        self.prepared_seek_landing.take_override_handoff()
    }

    #[cfg(test)]
    pub(crate) fn insert_prepared_seek_landing_frame_for_tests(
        &mut self,
        target_position: MediaTime,
        actual_pts: TrackTimestamp,
        lease: VideoFrameLease,
        branch_token: Option<PreparedSeekBranchToken>,
    ) {
        let video_track_id = self
            .pipeline
            .selected_video_track_id()
            .expect("prepared seek test requires selected video track");
        let target_pts = self.seek_landing_target_pts(video_track_id, target_position);
        let track_selection = self.seek_landing_track_selection_for_tests(video_track_id);
        let generation = self
            .pipeline
            .seek_generation()
            .checked_add(1)
            .expect("prepared seek test generation must not overflow");
        let key = TimelineHoverPrepareFrameKey::new(
            SourceRevision::new(SEEK_LANDING_SOURCE_REVISION_UNTRACKED),
            track_selection,
            BackendRevision::new(SEEK_LANDING_BACKEND_REVISION_UNTRACKED),
            seek_landing_prepared_generation_token(generation),
            FrameExactnessPolicy::TargetOrAfter,
            prepared_seek_landing_bucket(target_pts),
        );

        self.prepared_seek_landing.insert_prepared_frame_for_tests(
            key,
            lease,
            actual_pts,
            branch_token,
        );
    }

    #[cfg(test)]
    pub(crate) fn prepared_seek_landing_working_set_len_for_tests(&self) -> usize {
        self.prepared_seek_landing.working_set_len_for_tests()
    }

    #[cfg(test)]
    pub(crate) fn active_prepared_seek_landing_kind_for_tests(
        &self,
    ) -> Option<PreparedSeekLandingPromotionKind> {
        self.prepared_seek_landing.active_promotion_kind_for_tests()
    }

    #[cfg(test)]
    fn seek_landing_track_selection_for_tests(
        &self,
        video_track_id: crate::TrackId,
    ) -> ScrubTrackSelection {
        match self.pipeline.selected_audio_track_id() {
            Some(audio_track_id) => ScrubTrackSelection::with_audio(video_track_id, audio_track_id),
            None => ScrubTrackSelection::video_only(video_track_id),
        }
    }
}

/// Создаёт context, совпадающий с первым context нового one-shot SeekLanding driver-а.
#[must_use]
pub(super) fn prepared_seek_landing_context(
    track_selection: ScrubTrackSelection,
    target: ScrubTarget,
    playback_generation: u64,
) -> ScrubTargetContext {
    ScrubTargetContext::new(
        SourceRevision::new(SEEK_LANDING_SOURCE_REVISION_UNTRACKED),
        BackendRevision::new(SEEK_LANDING_BACKEND_REVISION_UNTRACKED),
        track_selection,
        target,
        ScrubExactnessPolicy::ExactFrame,
        ScrubRequestKind::SeekLanding,
        seek_landing_prepared_generation_token(playback_generation),
    )
}

/// Строит lookup request так, чтобы bucket оставался только индексом, а не proof.
fn prepared_seek_landing_lookup_request(
    context: ScrubTargetContext,
) -> TimelineHoverPrepareFrameLookupRequest {
    let key = TimelineHoverPrepareFrameKey::new(
        context.source_revision(),
        context.track_selection(),
        context.backend_revision(),
        context.generation(),
        FrameExactnessPolicy::TargetOrAfter,
        prepared_seek_landing_bucket(context.target().target_pts),
    );

    TimelineHoverPrepareFrameLookupRequest::new(key, context.target().target_pts)
}

/// Индексирует target PTS по normalized timeline; actual exactness проверяет working set.
fn prepared_seek_landing_bucket(target_pts: TrackTimestamp) -> TimelineHoverFrameBucket {
    let micros = target_pts.to_media_time().as_duration().as_micros();
    let bucket = i64::try_from(micros).unwrap_or(i64::MAX);

    TimelineHoverFrameBucket::new(bucket)
}

/// Возвращает generation token первого one-shot SeekLanding context-а.
fn seek_landing_prepared_generation_token(playback_generation: u64) -> ScrubGenerationToken {
    ScrubGenerationToken::new(
        PlaybackGeneration::new(playback_generation),
        ScrubGeneration::new(SEEK_LANDING_PREPARED_SCRUB_GENERATION),
    )
}

/// ResumePending budget S17B оставляет на текущем player-core fallback policy.
fn prepared_audio_resume_budget() -> frame_server_core::AudioResumeBudgetMetadata {
    derive_audio_resume_timeout_budget(AudioResumeTimingInput::unknown(), Duration::ZERO).metadata
}

/// Мапит generation-start failure в player error без silent fallback-а.
fn player_error_from_seek_landing_generation_error(
    error: SeekLandingGenerationStartError,
) -> PlayerError {
    match error {
        SeekLandingGenerationStartError::GenerationOverflow => PlayerError::new(
            PlayerErrorKind::RuntimeError,
            "Seek generation overflow would break prepared SeekLanding stale-frame guards",
        ),
        SeekLandingGenerationStartError::Stale(reason) => PlayerError::new(
            PlayerErrorKind::SeekUnavailable,
            format!("Prepared SeekLanding stale generation rejected: {reason:?}"),
        ),
    }
}
