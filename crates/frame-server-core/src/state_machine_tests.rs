use std::time::Duration;

use codec_core::{VideoColorMetadata, VideoDisplayOrientation};
use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};
use video_core::{DecodedFrame, FrameResourceHandle, VideoFrameDiagnostics};
use video_frame_contract::{DmaBufImageLayout, VideoFrameContract};
use video_present_core::{VideoPresentFrameIdentity, VideoPresentFrameResourceDescriptor};

use crate::DecodePointSeekedOutcome;
use crate::{
    AudioResumeBudgetMetadata, AudioResumeTimedOutOutcome, BackendRevision, CancelScrubReason,
    DecoderBackpressureOutcome, DecoderBackpressureReason, DemuxUnavailableOutcome,
    DemuxUnavailableReason, DemuxUnsupportedOutcome, DemuxUnsupportedReason, FatalOutcome,
    FinishScrubPolicy, FrameServerConfig, HostUploadBackpressureOutcome,
    HostUploadBackpressureReason, PlaybackGeneration, PreparedOutcome, ResourceBusyOutcome,
    ResourceBusyReason, ScrubCurrentGuards, ScrubDriverOutcome, ScrubEvent, ScrubExactnessPolicy,
    ScrubExecutionPolicy, ScrubFailedEvent, ScrubFailureReason, ScrubFatalReason, ScrubFrameTiming,
    ScrubGeneration, ScrubIntent, ScrubIntentKind, ScrubPreviewFrame, ScrubRequestKind,
    ScrubStaleReason, ScrubStateMachine, ScrubStep, ScrubTarget, ScrubTargetContext,
    ScrubTargetUpdate, ScrubTargetUpdateGuards, ScrubTimedOutOutcome, ScrubTimeoutReason,
    SourceRevision, StaleGenerationOutcome,
};

#[path = "state_machine_tests/exact_flow.rs"]
mod exact_flow;

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
    let resource_handle = FrameResourceHandle(42);
    ScrubPreviewFrame {
        generation: context.generation(),
        timing: ScrubFrameTiming::new(target.media_time, target.target_pts),
        frame_identity: frame_identity_for_tests(resource_handle),
        resource: descriptor_for_tests(resource_handle),
    }
}

fn descriptor_for_tests(
    resource_handle: FrameResourceHandle,
) -> VideoPresentFrameResourceDescriptor {
    let decoded_frame = decoded_frame_for_present_tests(resource_handle);
    VideoPresentFrameResourceDescriptor::from_decoded_frame(2, &decoded_frame)
}

fn frame_identity_for_tests(resource_handle: FrameResourceHandle) -> VideoPresentFrameIdentity {
    let decoded_frame = decoded_frame_for_present_tests(resource_handle);
    VideoPresentFrameIdentity::from_decoded_frame(2, &decoded_frame)
}

fn decoded_frame_for_present_tests(resource_handle: FrameResourceHandle) -> DecodedFrame {
    DecodedFrame {
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
    }
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
