#![allow(dead_code)]
// S13B добавляет driver skeleton до публичного command/UI wiring.
// Следующая сессия должна убрать этот allow, когда driver начнёт вызываться из session flow.

use std::collections::VecDeque;
use std::time::Duration;

use frame_server_core::{
    AudioResumeBudgetMetadata, AudioResumeErrorReason, CancelScrubReason, CancelledOutcome,
    DecodePointSeekedOutcome, DecoderBackpressureOutcome, DecoderBackpressureReason,
    DemuxUnavailableOutcome, DemuxUnavailableReason, DemuxUnsupportedOutcome,
    DemuxUnsupportedReason, ExactFrameReadyOutcome, FatalOutcome, FeedAndDrainStopCondition,
    FinishScrubPolicy, FinishedOutcome, HostUploadBackpressureOutcome,
    HostUploadBackpressureReason, MatchedPlaybackOutcome, PlaybackGeneration, PreparedOutcome,
    PreviewFrameReadyOutcome, ProgressedOutcome, ResourceBusyOutcome, ResourceBusyReason,
    ScrubDriverOutcome, ScrubEvent, ScrubEventFrameIdentity, ScrubExecutionPolicy,
    ScrubFatalReason, ScrubFrameTiming, ScrubGeneration, ScrubGenerationToken, ScrubIntent,
    ScrubIntentKind, ScrubNoVideoFrameReason, ScrubProgress, ScrubStaleReason, ScrubStateMachine,
    ScrubStep, ScrubTargetContext, ScrubTargetUpdate, ScrubTimedOutOutcome, ScrubTimeoutReason,
    SourceRevision, StaleGenerationOutcome, ValidatedFrameServerConfig,
};
use media_core::{DemuxSeekRequest, MediaDemuxError, MediaTime, TrackTimestamp};

use super::PlayerSession;
use crate::seek_state::demux_seek_request_for_transaction;
use crate::{PlayerError, SeekMode};

const AUDIO_RESUME_TIMEOUT_MAX: Duration = Duration::from_millis(500);
const AUDIO_RESUME_TIMEOUT_MARGIN: Duration = Duration::from_millis(25);
const DRIVER_INTENT_SAFETY_MARGIN: usize = 4;

/// Player-side driver, который исполняет neutral scrub machine над owner pipeline.
///
/// `frame-server-core` выдаёт только coarse intents. Этот driver держит реальный
/// lifecycle порядок у владельца demux/decoder/audio state, чтобы app/tick/frame
/// server не начали вручную повторять flush/generation/seek шаги.
pub(super) struct PlayerScrubTransactionDriver {
    state_machine: ScrubStateMachine,
}

impl PlayerScrubTransactionDriver {
    #[must_use]
    pub(super) fn new(
        config: ValidatedFrameServerConfig,
        initial_scrub_generation: ScrubGeneration,
    ) -> Self {
        Self {
            state_machine: ScrubStateMachine::new(config, initial_scrub_generation),
        }
    }

    /// Принимает новый target и сразу исполняет доступные machine intents.
    ///
    /// В S13B это skeleton: feed/drain может вернуть pending marker. Важно, что
    /// lifecycle order уже централизован здесь, а не размазан по command/tick/UI.
    pub(super) fn submit_target_update(
        &mut self,
        lifecycle: &mut impl ScrubTransactionLifecycle,
        update: ScrubTargetUpdate,
    ) -> ScrubDriverRun {
        let step = self.state_machine.submit_target_update(update);
        self.drive_step(lifecycle, step)
    }

    pub(super) fn cancel_active(
        &mut self,
        lifecycle: &mut impl ScrubTransactionLifecycle,
        reason: CancelScrubReason,
    ) -> ScrubDriverRun {
        let step = self.state_machine.cancel_active(reason);
        self.drive_step(lifecycle, step)
    }

    fn drive_step(
        &mut self,
        lifecycle: &mut impl ScrubTransactionLifecycle,
        first_step: ScrubStep,
    ) -> ScrubDriverRun {
        let mut run = ScrubDriverRun::default();
        let mut pending_intents = VecDeque::new();
        enqueue_step(first_step, &mut pending_intents, &mut run);

        let intent_budget = self
            .state_machine
            .config()
            .max_feed_and_drain_driver_steps() as usize
            + DRIVER_INTENT_SAFETY_MARGIN;
        let mut handled_intents = 0usize;

        while let Some(intent) = pending_intents.pop_front() {
            if handled_intents >= intent_budget {
                let context = *intent.context();
                let outcome = ScrubDriverOutcome::TimedOut(ScrubTimedOutOutcome {
                    context,
                    reason: ScrubTimeoutReason::DriverStepBudgetExceeded,
                    elapsed: Duration::ZERO,
                });
                record_outcome(self, outcome, &mut pending_intents, &mut run);
                break;
            }

            handled_intents += 1;
            run.intents.push(intent.kind());
            let outcome = execute_intent(
                lifecycle,
                intent,
                self.state_machine.current_scrub_generation(),
            );
            record_outcome(self, outcome, &mut pending_intents, &mut run);
        }

        run
    }
}

/// Сводка одного driver run для diagnostics/tests будущей интеграции.
#[derive(Debug, Default)]
pub(super) struct ScrubDriverRun {
    pub(super) intents: Vec<ScrubIntentKind>,
    pub(super) outcomes: Vec<ScrubDriverOutcome>,
    pub(super) events: Vec<ScrubEvent>,
}

/// Boundary, через который scrub driver трогает настоящий owner pipeline.
///
/// Здесь намеренно нет методов создания decoder/session: driver должен работать
/// только с уже установленными owner resources.
pub(super) trait ScrubTransactionLifecycle {
    fn current_playback_generation(&self) -> PlaybackGeneration;

    fn clear_old_decode_floor(&mut self, context: ScrubTargetContext) -> ScrubLifecycleResult<()>;

    fn flush_decoder(&mut self, context: ScrubTargetContext) -> ScrubLifecycleResult<()>;

    fn begin_nested_scrub_generation(
        &mut self,
        generation: ScrubGenerationToken,
    ) -> ScrubLifecycleResult<()>;

    fn clear_pending_queues(&mut self, context: ScrubTargetContext) -> ScrubLifecycleResult<()>;

    fn compute_decode_point_before(
        &mut self,
        context: ScrubTargetContext,
    ) -> ScrubLifecycleResult<ScrubDecodePointBefore>;

    fn seek_demux_to_decode_point(
        &mut self,
        context: ScrubTargetContext,
        decode_point: ScrubDecodePointBefore,
    ) -> ScrubLifecycleResult<ScrubDemuxSeekAccepted>;

    fn feed_and_drain(
        &mut self,
        context: ScrubTargetContext,
        stop_condition: FeedAndDrainStopCondition,
    ) -> ScrubLifecycleResult<ScrubFeedDrainResult>;

    fn finish_scrub(
        &mut self,
        context: ScrubTargetContext,
        policy: FinishScrubPolicy,
    ) -> ScrubLifecycleResult<ScrubFinishResult>;

    fn cancel_scrub(
        &mut self,
        context: ScrubTargetContext,
        reason: CancelScrubReason,
    ) -> ScrubLifecycleResult<()>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrubDecodePointBefore {
    pub(super) request: DemuxSeekRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct ScrubDemuxSeekAccepted {
    pub(super) actual_decode_time: MediaTime,
    pub(super) actual_decode_pts: TrackTimestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubFeedDrainResult {
    Progressed(ScrubProgress),
    ExactFrameReady(frame_server_core::ScrubPreviewFrame),
    PreviewFrameReady(frame_server_core::ScrubPreviewFrame),
    AudioResumePending(AudioResumeBudgetMetadata),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubFinishResult {
    Committed {
        committed_position: MediaTime,
        committed_frame_timing: ScrubFrameTiming,
        frame_identity: ScrubEventFrameIdentity,
    },
    MatchedPlayback {
        playback_position: MediaTime,
        matched_frame_timing: ScrubFrameTiming,
        frame_identity: ScrubEventFrameIdentity,
    },
    ReleasedWithoutCommit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ScrubLifecycleError {
    AudioResumeTimedOut(AudioResumeBudgetMetadata),
    AudioResumeFailed {
        reason: AudioResumeErrorReason,
        budget: Option<AudioResumeBudgetMetadata>,
    },
    Cancelled(CancelScrubReason),
    StaleGeneration(ScrubStaleReason),
    ResourceBusy(ResourceBusyReason),
    DemuxUnavailable(DemuxUnavailableReason),
    DemuxUnsupported(DemuxUnsupportedReason),
    DecoderBackpressure(DecoderBackpressureReason),
    HostUploadBackpressure(HostUploadBackpressureReason),
    TimedOut {
        reason: ScrubTimeoutReason,
        elapsed: Duration,
    },
    Fatal(ScrubFatalReason),
}

pub(super) type ScrubLifecycleResult<T> = Result<T, ScrubLifecycleError>;

/// Input, которым владеет player-core audio policy. `frame-server-core` этих
/// чисел не знает и получает только neutral budget metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioResumeTimingInput {
    pub(super) current_output_buffer: Option<Duration>,
    pub(super) callback_or_device_period: Option<Duration>,
}

impl AudioResumeTimingInput {
    #[must_use]
    pub(super) const fn known(
        current_output_buffer: Duration,
        callback_or_device_period: Duration,
    ) -> Self {
        Self {
            current_output_buffer: Some(current_output_buffer),
            callback_or_device_period: Some(callback_or_device_period),
        }
    }

    #[must_use]
    pub(super) const fn unknown() -> Self {
        Self {
            current_output_buffer: None,
            callback_or_device_period: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct AudioResumeTimeoutFormulaInputs {
    pub(super) current_output_buffer: Option<Duration>,
    pub(super) callback_or_device_period: Option<Duration>,
    pub(super) safety_margin: Duration,
    pub(super) max_budget: Duration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct PlayerAudioResumeBudget {
    pub(super) metadata: AudioResumeBudgetMetadata,
    pub(super) formula_inputs: AudioResumeTimeoutFormulaInputs,
}

#[must_use]
pub(super) fn derive_audio_resume_timeout_budget(
    timing: AudioResumeTimingInput,
    elapsed: Duration,
) -> PlayerAudioResumeBudget {
    let formula_inputs = AudioResumeTimeoutFormulaInputs {
        current_output_buffer: timing.current_output_buffer,
        callback_or_device_period: timing.callback_or_device_period,
        safety_margin: AUDIO_RESUME_TIMEOUT_MARGIN,
        max_budget: AUDIO_RESUME_TIMEOUT_MAX,
    };

    let Some(current_output_buffer) = timing.current_output_buffer else {
        return PlayerAudioResumeBudget {
            metadata: AudioResumeBudgetMetadata::timing_unknown_fallback(
                AUDIO_RESUME_TIMEOUT_MAX,
                elapsed,
            ),
            formula_inputs,
        };
    };
    let Some(callback_or_device_period) = timing.callback_or_device_period else {
        return PlayerAudioResumeBudget {
            metadata: AudioResumeBudgetMetadata::timing_unknown_fallback(
                AUDIO_RESUME_TIMEOUT_MAX,
                elapsed,
            ),
            formula_inputs,
        };
    };

    if callback_or_device_period.is_zero() {
        return PlayerAudioResumeBudget {
            metadata: AudioResumeBudgetMetadata::timing_unknown_fallback(
                AUDIO_RESUME_TIMEOUT_MAX,
                elapsed,
            ),
            formula_inputs,
        };
    }

    let formula_budget = current_output_buffer
        .checked_add(callback_or_device_period)
        .and_then(|partial| partial.checked_add(AUDIO_RESUME_TIMEOUT_MARGIN))
        .unwrap_or(AUDIO_RESUME_TIMEOUT_MAX)
        .min(AUDIO_RESUME_TIMEOUT_MAX);

    PlayerAudioResumeBudget {
        metadata: AudioResumeBudgetMetadata::supplied_by_driver(formula_budget, elapsed),
        formula_inputs,
    }
}

impl ScrubLifecycleError {
    pub(super) fn into_outcome(self, context: ScrubTargetContext) -> ScrubDriverOutcome {
        match self {
            Self::AudioResumeTimedOut(budget) => ScrubDriverOutcome::AudioResumeTimedOut(
                frame_server_core::AudioResumeTimedOutOutcome { context, budget },
            ),
            Self::AudioResumeFailed { reason, budget } => {
                ScrubDriverOutcome::AudioResumeFailed(frame_server_core::AudioResumeFailedOutcome {
                    context,
                    reason,
                    budget,
                })
            }
            Self::Cancelled(reason) => {
                ScrubDriverOutcome::Cancelled(CancelledOutcome { context, reason })
            }
            Self::StaleGeneration(reason) => {
                ScrubDriverOutcome::StaleGeneration(StaleGenerationOutcome { context, reason })
            }
            Self::ResourceBusy(reason) => {
                ScrubDriverOutcome::ResourceBusy(ResourceBusyOutcome { context, reason })
            }
            Self::DemuxUnavailable(reason) => {
                ScrubDriverOutcome::DemuxUnavailable(DemuxUnavailableOutcome { context, reason })
            }
            Self::DemuxUnsupported(reason) => {
                ScrubDriverOutcome::DemuxUnsupported(DemuxUnsupportedOutcome { context, reason })
            }
            Self::DecoderBackpressure(reason) => {
                ScrubDriverOutcome::DecoderBackpressure(DecoderBackpressureOutcome {
                    context,
                    reason,
                })
            }
            Self::HostUploadBackpressure(reason) => {
                ScrubDriverOutcome::HostUploadBackpressure(HostUploadBackpressureOutcome {
                    context,
                    reason,
                })
            }
            Self::TimedOut { reason, elapsed } => {
                ScrubDriverOutcome::TimedOut(ScrubTimedOutOutcome {
                    context,
                    reason,
                    elapsed,
                })
            }
            Self::Fatal(reason) => ScrubDriverOutcome::Fatal(FatalOutcome { context, reason }),
        }
    }
}

impl ScrubTransactionLifecycle for PlayerSession {
    fn current_playback_generation(&self) -> PlaybackGeneration {
        PlaybackGeneration::new(self.pipeline.seek_generation())
    }

    fn clear_old_decode_floor(&mut self, _context: ScrubTargetContext) -> ScrubLifecycleResult<()> {
        self.clear_active_seek_decoder_output_floor("scrub driver")
            .map_err(player_error_to_scrub_fatal)
    }

    fn flush_decoder(&mut self, _context: ScrubTargetContext) -> ScrubLifecycleResult<()> {
        self.reset_video_decoder_for_seek()
            .map_err(player_error_to_scrub_fatal)
    }

    fn begin_nested_scrub_generation(
        &mut self,
        generation: ScrubGenerationToken,
    ) -> ScrubLifecycleResult<()> {
        let current_generation = ScrubGenerationToken::new(
            self.current_playback_generation(),
            generation.scrub_generation,
        );
        if let Some(stale_reason) = generation.stale_reason_against(current_generation) {
            return Err(ScrubLifecycleError::StaleGeneration(stale_reason));
        }

        Ok(())
    }

    fn clear_pending_queues(&mut self, _context: ScrubTargetContext) -> ScrubLifecycleResult<()> {
        let has_video = self.pipeline.has_selected_video_track();
        self.pipeline.clear_pending_packets_for_seek();
        self.pipeline.reset_decoder_state_for_seek(has_video);
        self.clear_queued_video_frames();
        Ok(())
    }

    fn compute_decode_point_before(
        &mut self,
        context: ScrubTargetContext,
    ) -> ScrubLifecycleResult<ScrubDecodePointBefore> {
        if self.pipeline.selected_video_track_id() != Some(context.track_selection().video_track) {
            return Err(ScrubLifecycleError::DemuxUnsupported(
                DemuxUnsupportedReason::SelectedVideoTrackMissing,
            ));
        }

        let request = demux_seek_request_for_transaction(
            true, /* has_video_track */
            context.target().media_time.as_duration(),
            SeekMode::Accurate,
        )
        .map_err(|_error| {
            ScrubLifecycleError::DemuxUnsupported(
                DemuxUnsupportedReason::DecodePointBeforeUnsupported,
            )
        })?;

        Ok(ScrubDecodePointBefore { request })
    }

    fn seek_demux_to_decode_point(
        &mut self,
        context: ScrubTargetContext,
        decode_point: ScrubDecodePointBefore,
    ) -> ScrubLifecycleResult<ScrubDemuxSeekAccepted> {
        let Some(seek_result) = self.pipeline.seek_demuxer(decode_point.request) else {
            return Err(ScrubLifecycleError::DemuxUnavailable(
                DemuxUnavailableReason::DemuxerClosed,
            ));
        };

        let seek_result = seek_result.map_err(scrub_lifecycle_error_from_demux_seek_error)?;

        Ok(ScrubDemuxSeekAccepted {
            actual_decode_time: seek_result.actual_position,
            actual_decode_pts: seek_result
                .actual_track_timestamp
                .unwrap_or(context.target().target_pts),
        })
    }

    fn feed_and_drain(
        &mut self,
        _context: ScrubTargetContext,
        _stop_condition: FeedAndDrainStopCondition,
    ) -> ScrubLifecycleResult<ScrubFeedDrainResult> {
        let budget =
            derive_audio_resume_timeout_budget(AudioResumeTimingInput::unknown(), Duration::ZERO);
        Ok(ScrubFeedDrainResult::AudioResumePending(budget.metadata))
    }

    fn finish_scrub(
        &mut self,
        context: ScrubTargetContext,
        policy: FinishScrubPolicy,
    ) -> ScrubLifecycleResult<ScrubFinishResult> {
        let frame_timing = scrub_frame_timing_from_context(context);
        let frame_identity = self
            .current_present_frame_identity()
            .map(ScrubEventFrameIdentity::Video)
            .unwrap_or(ScrubEventFrameIdentity::NoVideoFrame(
                ScrubNoVideoFrameReason::CurrentFrameUnavailable,
            ));

        self.invalidate_in_flight_scrub_outputs_after_exit("scrub driver finish");

        match policy {
            FinishScrubPolicy::CommitVisiblePreview => Ok(ScrubFinishResult::Committed {
                committed_position: context.target().media_time,
                committed_frame_timing: frame_timing,
                frame_identity,
            }),
            FinishScrubPolicy::MatchPlaybackPosition => Ok(ScrubFinishResult::MatchedPlayback {
                playback_position: self.snapshot.timeline.current_position,
                matched_frame_timing: frame_timing,
                frame_identity,
            }),
            FinishScrubPolicy::ReleaseWithoutCommit => Ok(ScrubFinishResult::ReleasedWithoutCommit),
        }
    }

    fn cancel_scrub(
        &mut self,
        _context: ScrubTargetContext,
        reason: CancelScrubReason,
    ) -> ScrubLifecycleResult<()> {
        if reason != CancelScrubReason::SupersededByNewTarget {
            self.invalidate_in_flight_scrub_outputs_after_exit("scrub driver cancel");
        }

        Ok(())
    }
}

fn execute_intent(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    intent: ScrubIntent,
    current_scrub_generation: ScrubGeneration,
) -> ScrubDriverOutcome {
    if let Some(stale_reason) =
        scrub_intent_stale_reason(lifecycle, intent, current_scrub_generation)
    {
        return ScrubLifecycleError::StaleGeneration(stale_reason).into_outcome(*intent.context());
    }

    match intent {
        ScrubIntent::PrepareTarget(payload) => execute_prepare_target(lifecycle, payload.context),
        ScrubIntent::SeekDecodePointBefore(payload) => {
            execute_seek_decode_point_before(lifecycle, payload.context)
        }
        ScrubIntent::FeedAndDrain(payload) => {
            execute_feed_and_drain(lifecycle, payload.context, payload.stop_condition)
        }
        ScrubIntent::Finish(payload) => execute_finish(lifecycle, payload.context, payload.policy),
        ScrubIntent::Cancel(payload) => execute_cancel(lifecycle, payload.context, payload.reason),
    }
}

fn scrub_intent_stale_reason(
    lifecycle: &impl ScrubTransactionLifecycle,
    intent: ScrubIntent,
    current_scrub_generation: ScrubGeneration,
) -> Option<ScrubStaleReason> {
    let context_generation = intent.context().generation();
    let current_generation = ScrubGenerationToken::new(
        lifecycle.current_playback_generation(),
        current_scrub_generation,
    );
    let stale_reason = context_generation.stale_reason_against(current_generation)?;

    if superseded_cancel_can_release_old_target(intent, stale_reason) {
        return None;
    }

    Some(stale_reason)
}

fn superseded_cancel_can_release_old_target(
    intent: ScrubIntent,
    stale_reason: ScrubStaleReason,
) -> bool {
    matches!(
        intent,
        ScrubIntent::Cancel(payload)
            if payload.reason == CancelScrubReason::SupersededByNewTarget
    ) && matches!(
        stale_reason,
        ScrubStaleReason::ScrubGenerationMismatch { .. }
    )
}

fn execute_prepare_target(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    context: ScrubTargetContext,
) -> ScrubDriverOutcome {
    let prepare_result = lifecycle
        .clear_old_decode_floor(context)
        .and_then(|()| lifecycle.flush_decoder(context))
        .and_then(|()| lifecycle.begin_nested_scrub_generation(context.generation()))
        .and_then(|()| lifecycle.clear_pending_queues(context));

    match prepare_result {
        Ok(()) => ScrubDriverOutcome::Prepared(PreparedOutcome { context }),
        Err(error) => error.into_outcome(context),
    }
}

fn execute_seek_decode_point_before(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    context: ScrubTargetContext,
) -> ScrubDriverOutcome {
    let seek_result = lifecycle
        .compute_decode_point_before(context)
        .and_then(|decode_point| lifecycle.seek_demux_to_decode_point(context, decode_point));

    match seek_result {
        Ok(accepted) => ScrubDriverOutcome::DecodePointSeeked(DecodePointSeekedOutcome {
            context,
            actual_decode_time: accepted.actual_decode_time,
            actual_decode_pts: accepted.actual_decode_pts,
        }),
        Err(error) => error.into_outcome(context),
    }
}

fn execute_feed_and_drain(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    context: ScrubTargetContext,
    stop_condition: FeedAndDrainStopCondition,
) -> ScrubDriverOutcome {
    match lifecycle.feed_and_drain(context, stop_condition) {
        Ok(ScrubFeedDrainResult::Progressed(progress)) => {
            ScrubDriverOutcome::Progressed(ProgressedOutcome { context, progress })
        }
        Ok(ScrubFeedDrainResult::ExactFrameReady(frame)) => {
            ScrubDriverOutcome::ExactFrameReady(ExactFrameReadyOutcome { context, frame })
        }
        Ok(ScrubFeedDrainResult::PreviewFrameReady(frame)) => {
            ScrubDriverOutcome::PreviewFrameReady(PreviewFrameReadyOutcome { context, frame })
        }
        Ok(ScrubFeedDrainResult::AudioResumePending(budget)) => {
            ScrubDriverOutcome::AudioResumePending(frame_server_core::AudioResumePendingOutcome {
                context,
                budget,
            })
        }
        Err(error) => error.into_outcome(context),
    }
}

fn scrub_frame_timing_from_context(context: ScrubTargetContext) -> ScrubFrameTiming {
    let target = context.target();
    ScrubFrameTiming::new(target.media_time, target.target_pts)
}

fn execute_finish(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    context: ScrubTargetContext,
    policy: FinishScrubPolicy,
) -> ScrubDriverOutcome {
    match lifecycle.finish_scrub(context, policy) {
        Ok(ScrubFinishResult::Committed {
            committed_position,
            committed_frame_timing,
            frame_identity,
        }) => ScrubDriverOutcome::Finished(FinishedOutcome {
            context,
            committed_position,
            committed_frame_timing,
            frame_identity,
        }),
        Ok(ScrubFinishResult::MatchedPlayback {
            playback_position,
            matched_frame_timing,
            frame_identity,
        }) => ScrubDriverOutcome::MatchedPlayback(MatchedPlaybackOutcome {
            context,
            playback_position,
            matched_frame_timing,
            frame_identity,
        }),
        Ok(ScrubFinishResult::ReleasedWithoutCommit) => {
            ScrubDriverOutcome::Cancelled(CancelledOutcome {
                context,
                reason: CancelScrubReason::UserCancelled,
            })
        }
        Err(error) => error.into_outcome(context),
    }
}

fn execute_cancel(
    lifecycle: &mut impl ScrubTransactionLifecycle,
    context: ScrubTargetContext,
    reason: CancelScrubReason,
) -> ScrubDriverOutcome {
    match lifecycle.cancel_scrub(context, reason) {
        Ok(()) => ScrubDriverOutcome::Cancelled(CancelledOutcome { context, reason }),
        Err(error) => error.into_outcome(context),
    }
}

fn enqueue_step(
    step: ScrubStep,
    pending_intents: &mut VecDeque<ScrubIntent>,
    run: &mut ScrubDriverRun,
) {
    if let Some(event) = step.event() {
        run.events.push(event);
    }
    if let Some(intent) = step.first_intent() {
        pending_intents.push_back(intent);
    }
    if let Some(intent) = step.second_intent() {
        pending_intents.push_back(intent);
    }
}

fn record_outcome(
    driver: &mut PlayerScrubTransactionDriver,
    outcome: ScrubDriverOutcome,
    pending_intents: &mut VecDeque<ScrubIntent>,
    run: &mut ScrubDriverRun,
) {
    run.outcomes.push(outcome);
    let next_step = driver.state_machine.handle_driver_outcome(outcome);
    enqueue_step(next_step, pending_intents, run);
}

fn player_error_to_scrub_fatal(_error: PlayerError) -> ScrubLifecycleError {
    ScrubLifecycleError::Fatal(ScrubFatalReason::BackendContractViolated)
}

fn scrub_lifecycle_error_from_demux_seek_error(error: anyhow::Error) -> ScrubLifecycleError {
    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(|demux_error| {
                matches!(demux_error, MediaDemuxError::UnsupportedSeekMode { .. })
            })
    }) {
        return ScrubLifecycleError::DemuxUnsupported(
            DemuxUnsupportedReason::DecodePointBeforeUnsupported,
        );
    }

    if error.chain().any(|cause| {
        cause
            .downcast_ref::<MediaDemuxError>()
            .is_some_and(MediaDemuxError::is_seek_unavailable)
    }) {
        return ScrubLifecycleError::DemuxUnavailable(
            DemuxUnavailableReason::SeekableSourceMissing,
        );
    }

    ScrubLifecycleError::Fatal(ScrubFatalReason::BackendContractViolated)
}

#[must_use]
pub(super) fn scrub_update_guards_for_owner(
    source_revision: u64,
    backend_revision: u64,
    playback_generation: u64,
) -> frame_server_core::ScrubTargetUpdateGuards {
    frame_server_core::ScrubTargetUpdateGuards::new(
        SourceRevision::new(source_revision),
        frame_server_core::BackendRevision::new(backend_revision),
        PlaybackGeneration::new(playback_generation),
    )
}

#[must_use]
pub(super) fn default_scrub_execution_policy(
    config: ValidatedFrameServerConfig,
    finish_policy: FinishScrubPolicy,
) -> ScrubExecutionPolicy {
    ScrubExecutionPolicy::driver_step_limited(config, finish_policy)
}
