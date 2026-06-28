use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::VideoPresentFrameResourceDescriptor;

use crate::{
    AudioResumeBudgetMetadata, AudioResumeTimedOutOutcome, BackendRevision, CancelScrubReason,
    DecoderBackpressureOutcome, DecoderBackpressureReason, DemuxUnavailableOutcome,
    DemuxUnavailableReason, DemuxUnsupportedOutcome, DemuxUnsupportedReason, FatalOutcome,
    FeedAndDrainStopCondition, FinishScrubPolicy, FrameServerConfig, HostUploadBackpressureOutcome,
    HostUploadBackpressureReason, PlaybackGeneration, PreparedOutcome, PreviewFrameReadyOutcome,
    ResourceBusyOutcome, ResourceBusyReason, ScrubCurrentGuards, ScrubDriverOutcome, ScrubEvent,
    ScrubExactnessPolicy, ScrubExecutionPolicy, ScrubFailedEvent, ScrubFailureReason,
    ScrubFatalReason, ScrubGeneration, ScrubGenerationToken, ScrubIntent, ScrubIntentKind,
    ScrubPreviewFrame, ScrubProgress, ScrubProtocolPhase, ScrubRequestKind, ScrubStaleReason,
    ScrubStateMachine, ScrubStep, ScrubTarget, ScrubTargetContext, ScrubTargetReachStatus,
    ScrubTargetUpdate, ScrubTargetUpdateGuards, ScrubTimedOutOutcome, ScrubTimeoutReason,
    SourceRevision, StaleGenerationOutcome,
};
use crate::{DecodePointSeekedOutcome, FeedAndDrainIntent, FinishedOutcome, ProgressedOutcome};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeLifecycleStep {
    PrepareTarget,
    FlushDecoder,
    ClearQueues,
    DemuxSeekDecodePointBefore,
    FeedAndDrain,
    Finish,
    Cancel,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeDriverRejection {
    ContextMismatch {
        expected: ScrubTargetContext,
        actual: ScrubTargetContext,
    },
    StaleGuard(ScrubStaleReason),
}

struct FakeScrubTransactionDriver {
    expected_context: ScrubTargetContext,
    lifecycle_steps: Vec<FakeLifecycleStep>,
}

impl FakeScrubTransactionDriver {
    fn new(expected_context: ScrubTargetContext) -> Self {
        Self {
            expected_context,
            lifecycle_steps: Vec::new(),
        }
    }

    fn execute_intent(
        &mut self,
        intent: ScrubIntent,
        current_guards: ScrubCurrentGuards,
    ) -> Result<ScrubDriverOutcome, FakeDriverRejection> {
        let actual_context = *intent.context();
        if actual_context != self.expected_context {
            return Err(FakeDriverRejection::ContextMismatch {
                expected: self.expected_context,
                actual: actual_context,
            });
        }

        if let Some(stale_reason) = intent.stale_reason_against(current_guards) {
            return Err(FakeDriverRejection::StaleGuard(stale_reason));
        }

        Ok(match intent {
            ScrubIntent::PrepareTarget(_) => {
                self.lifecycle_steps.push(FakeLifecycleStep::PrepareTarget);
                ScrubDriverOutcome::Prepared(PreparedOutcome {
                    context: actual_context,
                })
            }
            ScrubIntent::SeekDecodePointBefore(_) => {
                self.lifecycle_steps.push(FakeLifecycleStep::FlushDecoder);
                self.lifecycle_steps.push(FakeLifecycleStep::ClearQueues);
                self.lifecycle_steps
                    .push(FakeLifecycleStep::DemuxSeekDecodePointBefore);
                ScrubDriverOutcome::DecodePointSeeked(decode_point_seeked_outcome(actual_context))
            }
            ScrubIntent::FeedAndDrain(_) => {
                self.lifecycle_steps.push(FakeLifecycleStep::FeedAndDrain);
                ScrubDriverOutcome::Progressed(ProgressedOutcome {
                    context: actual_context,
                    progress: ScrubProgress {
                        packets_fed: 3,
                        frames_drained: 1,
                        target_status: ScrubTargetReachStatus::BeforeTarget,
                    },
                })
            }
            ScrubIntent::Finish(_) => {
                self.lifecycle_steps.push(FakeLifecycleStep::Finish);
                ScrubDriverOutcome::Finished(FinishedOutcome {
                    context: actual_context,
                    committed_time: actual_context.target().media_time,
                })
            }
            ScrubIntent::Cancel(payload) => {
                self.lifecycle_steps.push(FakeLifecycleStep::Cancel);
                ScrubDriverOutcome::Cancelled(crate::CancelledOutcome {
                    context: actual_context,
                    reason: payload.reason,
                })
            }
        })
    }
}

#[test]
fn emits_prepare_seek_and_feed_intents_in_protocol_order() {
    let mut machine = ScrubStateMachine::default();
    let target_update = update_for_tests(ScrubRequestKind::LiveScrub, 1_000);

    let prepare_step = machine.submit_target_update(target_update);
    let prepare_intent = only_first_intent(prepare_step);
    assert_eq!(prepare_intent.kind(), ScrubIntentKind::PrepareTarget);
    assert_eq!(
        machine.active_phase(),
        Some(ScrubProtocolPhase::PreparingTarget)
    );

    let prepared_context = *prepare_intent.context();
    let mut driver = FakeScrubTransactionDriver::new(prepared_context);
    let prepared_outcome = driver
        .execute_intent(prepare_intent, guards_for_context(prepared_context))
        .expect("prepare context must satisfy fake driver guards");

    let seek_step = machine.handle_driver_outcome(prepared_outcome);
    assert!(matches!(seek_step.event(), Some(ScrubEvent::Started(_))));
    let seek_intent = only_first_intent(seek_step);
    assert_eq!(seek_intent.kind(), ScrubIntentKind::SeekDecodePointBefore);
    assert_eq!(*seek_intent.context(), prepared_context);
    assert_eq!(
        machine.active_phase(),
        Some(ScrubProtocolPhase::SeekingDecodePoint)
    );

    let seek_outcome = driver
        .execute_intent(seek_intent, guards_for_context(prepared_context))
        .expect("seek context must satisfy fake driver guards");
    let decode_anchor = match seek_outcome {
        ScrubDriverOutcome::DecodePointSeeked(payload) => payload,
        other => panic!("expected DecodePointSeeked, got {other:?}"),
    };
    assert_eq!(decode_anchor.context, prepared_context);
    assert_eq!(
        decode_anchor.actual_decode_time,
        MediaTime::from_millis(900)
    );
    assert_eq!(
        decode_anchor.actual_decode_pts,
        track_timestamp(prepared_context.track_selection().video_track, 900)
    );

    let feed_step =
        machine.handle_driver_outcome(ScrubDriverOutcome::DecodePointSeeked(decode_anchor));
    assert!(matches!(feed_step.event(), Some(ScrubEvent::Progress(_))));
    let feed_intent = only_first_intent(feed_step);
    assert_eq!(feed_intent.kind(), ScrubIntentKind::FeedAndDrain);
    assert_eq!(
        feed_intent,
        ScrubIntent::FeedAndDrain(FeedAndDrainIntent {
            context: prepared_context,
            stop_condition: FeedAndDrainStopCondition::DriverStepLimit { max_steps: 256 },
        })
    );
    assert_eq!(
        machine.active_phase(),
        Some(ScrubProtocolPhase::FeedingAndDraining)
    );

    assert_lifecycle_before(
        &driver.lifecycle_steps,
        FakeLifecycleStep::FlushDecoder,
        FakeLifecycleStep::DemuxSeekDecodePointBefore,
    );
    assert_lifecycle_before(
        &driver.lifecycle_steps,
        FakeLifecycleStep::ClearQueues,
        FakeLifecycleStep::DemuxSeekDecodePointBefore,
    );
}

#[test]
fn accepted_public_intents_stay_coarse_and_lifecycle_steps_stay_fake_driver_private() {
    assert_eq!(
        ScrubIntent::accepted_kinds(),
        &[
            ScrubIntentKind::PrepareTarget,
            ScrubIntentKind::SeekDecodePointBefore,
            ScrubIntentKind::FeedAndDrain,
            ScrubIntentKind::Finish,
            ScrubIntentKind::Cancel,
        ]
    );
}

#[test]
fn fake_driver_rejects_stale_and_mismatched_context_before_outcome() {
    let mut machine = ScrubStateMachine::default();
    let first_intent = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let context = *first_intent.context();
    let mut driver = FakeScrubTransactionDriver::new(context);

    let stale_generation = ScrubGenerationToken::new(
        context.generation().playback_generation,
        ScrubGeneration::new(context.generation().scrub_generation.get() + 1),
    );
    let stale_guards = ScrubCurrentGuards::new(
        context.source_revision(),
        context.backend_revision(),
        stale_generation,
    );
    let stale_result = driver.execute_intent(first_intent, stale_guards);
    assert!(matches!(
        stale_result,
        Err(FakeDriverRejection::StaleGuard(
            ScrubStaleReason::ScrubGenerationMismatch { .. }
        ))
    ));
    assert!(driver.lifecycle_steps.is_empty());

    let replacement_step =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 2_000));
    let replacement_context = *replacement_step
        .second_intent()
        .expect("replacement step must prepare the latest target")
        .context();
    let mut replacement_driver = FakeScrubTransactionDriver::new(replacement_context);
    let mismatch_result =
        replacement_driver.execute_intent(first_intent, guards_for_context(context));
    assert!(matches!(
        mismatch_result,
        Err(FakeDriverRejection::ContextMismatch { .. })
    ));
    assert!(replacement_driver.lifecycle_steps.is_empty());
}

#[test]
fn rich_outcomes_keep_typed_public_failure_categories() {
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::DecoderBackpressure(DecoderBackpressureOutcome {
                context,
                reason: DecoderBackpressureReason::PacketQueueFull,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::DecoderBackpressure),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::HostUploadBackpressure(HostUploadBackpressureOutcome {
                context,
                reason: HostUploadBackpressureReason::UploadSlotsExhausted,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::HostUploadBackpressure),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::ResourceBusy(ResourceBusyOutcome {
                context,
                reason: ResourceBusyReason::BackendResourcePressure,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::ResourceBusy),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::StaleGeneration(StaleGenerationOutcome {
                context,
                reason: ScrubStaleReason::ScrubGenerationMismatch {
                    context_generation: ScrubGeneration::new(1),
                    current_generation: ScrubGeneration::new(2),
                },
            })
        },
        ExpectedTerminalEvent::Cancelled,
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::DemuxUnsupported(DemuxUnsupportedOutcome {
                context,
                reason: DemuxUnsupportedReason::DecodePointBeforeUnsupported,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::DemuxUnsupported),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::DemuxUnavailable(DemuxUnavailableOutcome {
                context,
                reason: DemuxUnavailableReason::DemuxerClosed,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::DemuxUnavailable),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::AudioResumeTimedOut(AudioResumeTimedOutOutcome {
                context,
                budget: AudioResumeBudgetMetadata::timing_unknown_fallback(
                    Duration::from_millis(250),
                    Duration::from_millis(251),
                ),
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::AudioResumeTimedOut),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::TimedOut(ScrubTimedOutOutcome {
                context,
                reason: ScrubTimeoutReason::DriverStepBudgetExceeded,
                elapsed: Duration::from_millis(500),
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::Timeout),
    );
    assert_terminal_event(
        |context| {
            ScrubDriverOutcome::Fatal(FatalOutcome {
                context,
                reason: ScrubFatalReason::DriverInvariantViolated,
            })
        },
        ExpectedTerminalEvent::Failed(ScrubFailureReason::Fatal),
    );
}

#[test]
fn preview_frame_ready_finishes_and_commits_without_driver_details_in_public_event() {
    let mut machine = ScrubStateMachine::default();
    let prepare_context = context_from_step(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );

    let seek_step = machine.handle_driver_outcome(ScrubDriverOutcome::Prepared(PreparedOutcome {
        context: prepare_context,
    }));
    assert_eq!(
        seek_step
            .first_intent()
            .expect("prepared outcome must request decode-point seek")
            .kind(),
        ScrubIntentKind::SeekDecodePointBefore
    );

    let feed_step = machine.handle_driver_outcome(ScrubDriverOutcome::DecodePointSeeked(
        decode_point_seeked_outcome(prepare_context),
    ));
    assert_eq!(
        feed_step
            .first_intent()
            .expect("decode-point seek must request feed/drain")
            .kind(),
        ScrubIntentKind::FeedAndDrain
    );

    let preview_frame = preview_frame_for_tests(prepare_context);
    let finish_step = machine.handle_driver_outcome(ScrubDriverOutcome::PreviewFrameReady(
        PreviewFrameReadyOutcome {
            context: prepare_context,
            frame: preview_frame,
        },
    ));

    assert!(matches!(
        finish_step.event(),
        Some(ScrubEvent::PreviewFrameReady(event))
            if event.context == prepare_context && event.frame == preview_frame
    ));
    assert_eq!(
        finish_step.first_intent(),
        Some(ScrubIntent::Finish(crate::FinishScrubIntent {
            context: prepare_context,
            policy: FinishScrubPolicy::CommitVisiblePreview,
        }))
    );
    assert_eq!(machine.active_phase(), Some(ScrubProtocolPhase::Finishing));

    let committed_step =
        machine.handle_driver_outcome(ScrubDriverOutcome::Finished(FinishedOutcome {
            context: prepare_context,
            committed_time: prepare_context.target().media_time,
        }));

    assert!(matches!(
        committed_step.event(),
        Some(ScrubEvent::Committed(event))
            if event.context == prepare_context
                && event.committed_time == prepare_context.target().media_time
    ));
    assert!(committed_step.first_intent().is_none());
    assert_eq!(machine.active_context(), None);
}

#[test]
fn new_target_increments_generation_cancels_old_intent_and_ignores_old_outcome() {
    let mut machine = ScrubStateMachine::default();

    let first_step =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000));
    let first_context = *only_first_intent(first_step).context();
    assert_eq!(
        first_context.generation().scrub_generation,
        ScrubGeneration::new(1)
    );

    let second_step =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 2_000));
    let cancel_intent = second_step
        .first_intent()
        .expect("new latest target must cancel old target");
    let second_prepare = second_step
        .second_intent()
        .expect("new latest target must prepare replacement");
    let second_context = *second_prepare.context();

    assert_eq!(
        cancel_intent,
        ScrubIntent::Cancel(crate::CancelScrubIntent {
            context: first_context,
            reason: CancelScrubReason::SupersededByNewTarget,
        })
    );
    assert_eq!(second_prepare.kind(), ScrubIntentKind::PrepareTarget);
    assert_eq!(
        second_context.generation().scrub_generation,
        ScrubGeneration::new(2)
    );
    assert_eq!(machine.active_context(), Some(second_context));

    let stale_old_outcome = ScrubDriverOutcome::Prepared(PreparedOutcome {
        context: first_context,
    });
    assert!(machine.handle_driver_outcome(stale_old_outcome).is_idle());
    assert_eq!(machine.active_context(), Some(second_context));
}

#[test]
fn live_scrub_suspends_hover_targets_until_release_or_cancel() {
    let mut machine = ScrubStateMachine::default();

    let hover_prepare = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::HoverPreview, 1_000)),
    );
    let hover_context = *hover_prepare.context();

    let live_start =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 2_000));
    assert_eq!(
        live_start.first_intent(),
        Some(ScrubIntent::Cancel(crate::CancelScrubIntent {
            context: hover_context,
            reason: CancelScrubReason::SupersededByNewTarget,
        }))
    );
    let live_context = *live_start
        .second_intent()
        .expect("live scrub must prepare after cancelling hover")
        .context();
    assert_eq!(live_context.request_kind(), ScrubRequestKind::LiveScrub);
    assert!(machine.live_scrub_owns_target_stream());

    let ignored_hover = machine.submit_target_update(update_for_tests(
        ScrubRequestKind::TimelineHoverPrepareWindow,
        3_000,
    ));
    assert!(ignored_hover.is_idle());
    assert_eq!(machine.active_context(), Some(live_context));

    let cancel_live = machine.cancel_active(CancelScrubReason::UserCancelled);
    assert!(matches!(
        cancel_live.event(),
        Some(ScrubEvent::Cancelled(_))
    ));
    assert_eq!(
        cancel_live.first_intent(),
        Some(ScrubIntent::Cancel(crate::CancelScrubIntent {
            context: live_context,
            reason: CancelScrubReason::UserCancelled,
        }))
    );
    assert!(!machine.live_scrub_owns_target_stream());

    let resumed_hover = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::HoverPreview, 4_000)),
    );
    assert_eq!(
        resumed_hover.context().request_kind(),
        ScrubRequestKind::HoverPreview
    );
    assert_ne!(
        resumed_hover.context().generation(),
        live_context.generation()
    );
}

#[test]
fn cancellation_emits_cancelled_event_and_terminal_cancel_intent() {
    let mut machine = ScrubStateMachine::default();
    let prepare = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::SeekLanding, 1_000)),
    );
    let context = *prepare.context();

    let cancelled = machine.cancel_active(CancelScrubReason::UserCancelled);

    assert!(matches!(cancelled.event(), Some(ScrubEvent::Cancelled(_))));
    assert_eq!(
        cancelled.first_intent(),
        Some(ScrubIntent::Cancel(crate::CancelScrubIntent {
            context,
            reason: CancelScrubReason::UserCancelled,
        }))
    );
    assert!(cancelled.second_intent().is_none());
    assert_eq!(machine.active_context(), None);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedTerminalEvent {
    Cancelled,
    Failed(ScrubFailureReason),
}

fn assert_terminal_event(
    outcome_builder: impl FnOnce(ScrubTargetContext) -> ScrubDriverOutcome,
    expected: ExpectedTerminalEvent,
) {
    let mut machine = ScrubStateMachine::default();
    let context = context_from_step(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );

    let step = machine.handle_driver_outcome(outcome_builder(context));
    let event = step.event().expect("terminal outcome must emit event");
    assert!(step.first_intent().is_none());
    assert!(step.second_intent().is_none());
    assert_eq!(machine.active_context(), None);

    match expected {
        ExpectedTerminalEvent::Cancelled => {
            assert!(matches!(event, ScrubEvent::Cancelled(_)));
        }
        ExpectedTerminalEvent::Failed(expected_reason) => {
            let ScrubEvent::Failed(ScrubFailedEvent { reason, .. }) = event else {
                panic!("expected Failed event, got {event:?}");
            };
            assert_eq!(reason, expected_reason);
            if expected_reason != ScrubFailureReason::Fatal {
                assert_ne!(reason, ScrubFailureReason::Fatal);
            }
        }
    }
}

fn update_for_tests(request_kind: ScrubRequestKind, millis: u64) -> ScrubTargetUpdate {
    let video_track = TrackId::new(7);
    let config = FrameServerConfig::default()
        .validate()
        .expect("default config must be valid");
    ScrubTargetUpdate::new(
        ScrubTargetUpdateGuards::new(
            SourceRevision::new(10),
            BackendRevision::new(20),
            PlaybackGeneration::new(30),
        ),
        crate::ScrubTrackSelection::with_audio(video_track, TrackId::new(8)),
        target_for_tests(video_track, millis),
        ScrubExactnessPolicy::TargetOrAfter,
        request_kind,
        ScrubExecutionPolicy::driver_step_limited(config, FinishScrubPolicy::CommitVisiblePreview),
    )
}

fn target_for_tests(track_id: TrackId, millis: u64) -> ScrubTarget {
    ScrubTarget::new(
        MediaTime::from_millis(millis),
        track_timestamp(track_id, millis),
    )
}

fn decode_point_seeked_outcome(context: ScrubTargetContext) -> DecodePointSeekedOutcome {
    let decode_anchor_millis = 900;
    DecodePointSeekedOutcome {
        context,
        actual_decode_time: MediaTime::from_millis(decode_anchor_millis),
        actual_decode_pts: track_timestamp(
            context.track_selection().video_track,
            decode_anchor_millis,
        ),
    }
}

fn preview_frame_for_tests(context: ScrubTargetContext) -> ScrubPreviewFrame {
    let target = context.target();
    ScrubPreviewFrame {
        generation: context.generation(),
        actual_time: target.media_time,
        actual_pts: target.target_pts,
        resource: descriptor_for_tests(FrameResourceHandle(42)),
    }
}

fn descriptor_for_tests(
    resource_handle: FrameResourceHandle,
) -> VideoPresentFrameResourceDescriptor {
    let decoded_frame = DecodedFrame {
        generation: 30,
        pts: Duration::from_millis(1_250),
        frame_contract: VideoFrameContract::dma_buf_nv12(DmaBufImageLayout::SeparateLayers),
        width: 640,
        height: 360,
        render_width: 640,
        render_height: 360,
        display_orientation: VideoDisplayOrientation::Identity,
        color: VideoColorMetadata::sdr_bt709_limited(),
        resource_handle,
        diagnostics: VideoFrameDiagnostics::default(),
    };

    VideoPresentFrameResourceDescriptor::from_decoded_frame(2, &decoded_frame)
}

fn track_timestamp(track_id: TrackId, millis: u64) -> TrackTimestamp {
    let time_base = TimeBase::new(1, 1_000).expect("valid test timebase");
    TrackTimestamp::new(track_id, millis as i64, time_base)
}

fn guards_for_context(context: ScrubTargetContext) -> ScrubCurrentGuards {
    ScrubCurrentGuards::new(
        context.source_revision(),
        context.backend_revision(),
        context.generation(),
    )
}

fn only_first_intent(step: ScrubStep) -> ScrubIntent {
    let intent = step.first_intent().expect("step must contain first intent");
    assert!(step.second_intent().is_none());
    intent
}

fn context_from_step(step: ScrubStep) -> ScrubTargetContext {
    *step
        .first_intent()
        .expect("step must contain context-carrying intent")
        .context()
}

fn assert_lifecycle_before(
    steps: &[FakeLifecycleStep],
    earlier_step: FakeLifecycleStep,
    later_step: FakeLifecycleStep,
) {
    let earlier_index = steps
        .iter()
        .position(|step| *step == earlier_step)
        .expect("earlier lifecycle step must be recorded");
    let later_index = steps
        .iter()
        .position(|step| *step == later_step)
        .expect("later lifecycle step must be recorded");
    assert!(
        earlier_index < later_index,
        "{earlier_step:?} must happen before {later_step:?}, actual steps: {steps:?}"
    );
}
