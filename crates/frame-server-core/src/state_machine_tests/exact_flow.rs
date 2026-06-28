use std::cmp::Ordering;
use std::collections::VecDeque;

use media_core::MediaTime;
use video_core::FrameResourceHandle;

use super::{
    decode_point_seeked_outcome, descriptor_for_tests, guards_for_context, only_first_intent,
    preview_frame_for_tests, track_timestamp, update_for_tests,
};
use crate::{
    CancelScrubReason, ExactFrameReadyOutcome, FeedAndDrainIntent, FeedAndDrainStopCondition,
    FinishScrubPolicy, FinishedOutcome, PreTargetReleasedOutcome, PreparedOutcome,
    ScrubCurrentGuards, ScrubDriverOutcome, ScrubEvent, ScrubGeneration, ScrubGenerationToken,
    ScrubIntent, ScrubIntentKind, ScrubPreviewFrame, ScrubProgress, ScrubProtocolPhase,
    ScrubRequestKind, ScrubStaleReason, ScrubStateMachine, ScrubTargetContext,
    ScrubTargetReachStatus,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FakeDecoderHandle(u64);

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
    DemuxSeekBeforePrepareLifecycle,
    StaleGuard(ScrubStaleReason),
}

struct FakeScrubTransactionDriver {
    expected_context: ScrubTargetContext,
    prepared_context: Option<ScrubTargetContext>,
    decoder_handle: FakeDecoderHandle,
    created_decoder_handles: Vec<FakeDecoderHandle>,
    flush_decoder_handles: Vec<FakeDecoderHandle>,
    lifecycle_steps: Vec<FakeLifecycleStep>,
    decoded_frames: VecDeque<ScrubPreviewFrame>,
    released_pre_target_frames: Vec<ScrubPreviewFrame>,
    published_ready_frames: Vec<ScrubPreviewFrame>,
}

impl FakeScrubTransactionDriver {
    fn new(expected_context: ScrubTargetContext) -> Self {
        let decoder_handle = FakeDecoderHandle(1);
        Self {
            expected_context,
            prepared_context: None,
            decoder_handle,
            created_decoder_handles: vec![decoder_handle],
            flush_decoder_handles: Vec::new(),
            lifecycle_steps: Vec::new(),
            decoded_frames: default_decoded_frames(expected_context),
            released_pre_target_frames: Vec::new(),
            published_ready_frames: Vec::new(),
        }
    }

    fn expect_context(&mut self, expected_context: ScrubTargetContext) {
        self.expected_context = expected_context;
        self.prepared_context = None;
        self.decoded_frames = default_decoded_frames(expected_context);
    }

    fn prepare_lifecycle_completed(&self, context: ScrubTargetContext) -> bool {
        if self.prepared_context != Some(context) {
            return false;
        }

        lifecycle_step_last_index(&self.lifecycle_steps, FakeLifecycleStep::PrepareTarget)
            .zip(lifecycle_step_last_index(
                &self.lifecycle_steps,
                FakeLifecycleStep::FlushDecoder,
            ))
            .zip(lifecycle_step_last_index(
                &self.lifecycle_steps,
                FakeLifecycleStep::ClearQueues,
            ))
            .is_some_and(|((prepare_index, flush_index), clear_index)| {
                prepare_index < flush_index && flush_index < clear_index
            })
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
                self.flush_decoder_handles.push(self.decoder_handle);
                self.lifecycle_steps.push(FakeLifecycleStep::FlushDecoder);
                self.lifecycle_steps.push(FakeLifecycleStep::ClearQueues);
                self.prepared_context = Some(actual_context);
                ScrubDriverOutcome::Prepared(PreparedOutcome {
                    context: actual_context,
                })
            }
            ScrubIntent::SeekDecodePointBefore(_) => {
                if !self.prepare_lifecycle_completed(actual_context) {
                    return Err(FakeDriverRejection::DemuxSeekBeforePrepareLifecycle);
                }

                self.lifecycle_steps
                    .push(FakeLifecycleStep::DemuxSeekDecodePointBefore);
                ScrubDriverOutcome::DecodePointSeeked(decode_point_seeked_outcome(actual_context))
            }
            ScrubIntent::FeedAndDrain(_) => {
                self.lifecycle_steps.push(FakeLifecycleStep::FeedAndDrain);
                let decoded_frame = self
                    .decoded_frames
                    .pop_front()
                    .expect("fake driver must have a decoded frame for feed/drain");

                if decoded_frame
                    .actual_pts
                    .cmp_timeline_position(actual_context.target().target_pts)
                    == Ordering::Less
                {
                    self.released_pre_target_frames.push(decoded_frame);
                    ScrubDriverOutcome::PreTargetReleased(PreTargetReleasedOutcome {
                        context: actual_context,
                        released_frame: decoded_frame,
                        progress: ScrubProgress {
                            packets_fed: 1,
                            frames_drained: 1,
                            target_status: ScrubTargetReachStatus::BeforeTarget,
                        },
                    })
                } else {
                    self.published_ready_frames.push(decoded_frame);
                    ScrubDriverOutcome::ExactFrameReady(ExactFrameReadyOutcome {
                        context: actual_context,
                        frame: decoded_frame,
                    })
                }
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
fn fake_driver_rejects_demux_seek_before_prepare_flush_clear_lifecycle() {
    let mut machine = ScrubStateMachine::default();
    let prepare_intent = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let context = *prepare_intent.context();
    let mut driver = FakeScrubTransactionDriver::new(context);
    let illegal_seek_intent =
        ScrubIntent::SeekDecodePointBefore(crate::SeekDecodePointBeforeIntent { context });

    let result = driver.execute_intent(illegal_seek_intent, guards_for_context(context));

    assert_eq!(
        result,
        Err(FakeDriverRejection::DemuxSeekBeforePrepareLifecycle)
    );
    assert!(driver.lifecycle_steps.is_empty());
}

#[test]
fn fake_driver_reuses_same_decoder_handle_across_prepare_flushes() {
    let mut machine = ScrubStateMachine::default();
    let first_prepare = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let first_context = *first_prepare.context();
    let mut driver = FakeScrubTransactionDriver::new(first_context);

    driver
        .execute_intent(first_prepare, guards_for_context(first_context))
        .expect("first prepare must flush the existing decoder");

    let replacement_step =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 2_000));
    let second_prepare = replacement_step
        .second_intent()
        .expect("replacement update must prepare the new target");
    let second_context = *second_prepare.context();
    driver.expect_context(second_context);

    driver
        .execute_intent(second_prepare, guards_for_context(second_context))
        .expect("second prepare must reuse and flush the same decoder");

    assert_eq!(driver.created_decoder_handles, vec![FakeDecoderHandle(1)]);
    assert_eq!(
        driver.flush_decoder_handles,
        vec![FakeDecoderHandle(1), FakeDecoderHandle(1)]
    );
}

#[test]
fn exact_scrub_flow_releases_pre_target_then_finishes_on_first_target_or_after() {
    let mut machine = ScrubStateMachine::default();
    let prepare_intent = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let prepare_context = *prepare_intent.context();
    let mut driver = FakeScrubTransactionDriver::new(prepare_context);

    let prepared_outcome = driver
        .execute_intent(prepare_intent, guards_for_context(prepare_context))
        .expect("prepare must use fresh guards");
    let seek_step = machine.handle_driver_outcome(prepared_outcome);
    assert_eq!(
        seek_step
            .first_intent()
            .expect("prepared outcome must request decode-point seek")
            .kind(),
        ScrubIntentKind::SeekDecodePointBefore
    );
    assert_lifecycle_before(
        &driver.lifecycle_steps,
        FakeLifecycleStep::PrepareTarget,
        FakeLifecycleStep::FlushDecoder,
    );
    assert_lifecycle_before(
        &driver.lifecycle_steps,
        FakeLifecycleStep::FlushDecoder,
        FakeLifecycleStep::ClearQueues,
    );

    let seek_intent = only_first_intent(seek_step);
    let seek_outcome = driver
        .execute_intent(seek_intent, guards_for_context(prepare_context))
        .expect("demux seek must happen after fake prepare lifecycle");
    let feed_step = machine.handle_driver_outcome(seek_outcome);
    assert_eq!(
        feed_step
            .first_intent()
            .expect("decode-point seek must request feed/drain")
            .kind(),
        ScrubIntentKind::FeedAndDrain
    );
    assert_lifecycle_before(
        &driver.lifecycle_steps,
        FakeLifecycleStep::ClearQueues,
        FakeLifecycleStep::DemuxSeekDecodePointBefore,
    );

    let first_feed_intent = only_first_intent(feed_step);
    let pre_target_outcome = driver
        .execute_intent(first_feed_intent, guards_for_context(prepare_context))
        .expect("fake decoder must release the first pre-target frame");
    let pre_target_step = machine.handle_driver_outcome(pre_target_outcome);

    assert!(matches!(
        pre_target_step.event(),
        Some(ScrubEvent::Progress(event))
            if event.context == prepare_context
                && event.progress.target_status == ScrubTargetReachStatus::BeforeTarget
    ));
    assert_eq!(driver.released_pre_target_frames.len(), 1);
    assert!(driver.published_ready_frames.is_empty());
    assert_eq!(
        pre_target_step
            .first_intent()
            .expect("pre-target release must keep feeding until target")
            .kind(),
        ScrubIntentKind::FeedAndDrain
    );

    let second_feed_intent = only_first_intent(pre_target_step);
    let second_pre_target_outcome = driver
        .execute_intent(second_feed_intent, guards_for_context(prepare_context))
        .expect("fake decoder must release every pre-target frame");
    let second_pre_target_step = machine.handle_driver_outcome(second_pre_target_outcome);
    assert!(matches!(
        second_pre_target_step.event(),
        Some(ScrubEvent::Progress(event))
            if event.context == prepare_context
                && event.progress.target_status == ScrubTargetReachStatus::BeforeTarget
    ));
    assert_eq!(driver.released_pre_target_frames.len(), 2);
    assert!(driver.published_ready_frames.is_empty());

    let target_feed_intent = only_first_intent(second_pre_target_step);
    let exact_ready_outcome = driver
        .execute_intent(target_feed_intent, guards_for_context(prepare_context))
        .expect("fake decoder must publish the first target-or-after frame");
    let ready_frame = match exact_ready_outcome {
        ScrubDriverOutcome::ExactFrameReady(payload) => payload.frame,
        other => panic!("expected ExactFrameReady, got {other:?}"),
    };
    assert_eq!(ready_frame.actual_pts, prepare_context.target().target_pts);
    assert_eq!(driver.published_ready_frames, vec![ready_frame]);

    let finish_step = machine.handle_driver_outcome(ScrubDriverOutcome::ExactFrameReady(
        ExactFrameReadyOutcome {
            context: prepare_context,
            frame: ready_frame,
        },
    ));
    assert!(matches!(
        finish_step.event(),
        Some(ScrubEvent::PreviewFrameReady(event))
            if event.context == prepare_context && event.frame == ready_frame
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
fn late_exact_frame_after_replacement_is_ignored_and_not_published() {
    let mut machine = ScrubStateMachine::default();
    let first_prepare = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let first_context = *first_prepare.context();
    let first_frame = preview_frame_for_tests(first_context);

    let replacement_step =
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 2_000));
    let second_context = *replacement_step
        .second_intent()
        .expect("replacement update must prepare latest target")
        .context();

    let late_step = machine.handle_driver_outcome(ScrubDriverOutcome::ExactFrameReady(
        ExactFrameReadyOutcome {
            context: first_context,
            frame: first_frame,
        },
    ));

    assert!(late_step.is_idle());
    assert_eq!(machine.active_context(), Some(second_context));
}

#[test]
fn late_exact_frame_after_cancellation_is_ignored_and_not_published() {
    let mut machine = ScrubStateMachine::default();
    let prepare_intent = only_first_intent(
        machine.submit_target_update(update_for_tests(ScrubRequestKind::LiveScrub, 1_000)),
    );
    let context = *prepare_intent.context();
    let frame = preview_frame_for_tests(context);

    let cancel_step = machine.cancel_active(CancelScrubReason::UserCancelled);
    assert!(matches!(
        cancel_step.event(),
        Some(ScrubEvent::Cancelled(event))
            if event.context == context && event.reason == CancelScrubReason::UserCancelled
    ));

    let late_step = machine.handle_driver_outcome(ScrubDriverOutcome::ExactFrameReady(
        ExactFrameReadyOutcome { context, frame },
    ));

    assert!(late_step.is_idle());
    assert_eq!(machine.active_context(), None);
}

fn decoded_frame_for_tests(
    context: ScrubTargetContext,
    millis: u64,
    resource_handle: FrameResourceHandle,
) -> ScrubPreviewFrame {
    ScrubPreviewFrame {
        generation: context.generation(),
        actual_time: MediaTime::from_millis(millis),
        actual_pts: track_timestamp(context.track_selection().video_track, millis),
        resource: descriptor_for_tests(resource_handle),
    }
}

fn default_decoded_frames(context: ScrubTargetContext) -> VecDeque<ScrubPreviewFrame> {
    let target = context.target();
    VecDeque::from([
        decoded_frame_for_tests(context, 900, FrameResourceHandle(40)),
        decoded_frame_for_tests(context, 950, FrameResourceHandle(41)),
        ScrubPreviewFrame {
            generation: context.generation(),
            actual_time: target.media_time,
            actual_pts: target.target_pts,
            resource: descriptor_for_tests(FrameResourceHandle(42)),
        },
        decoded_frame_for_tests(context, 1_100, FrameResourceHandle(43)),
    ])
}

fn assert_lifecycle_before(
    steps: &[FakeLifecycleStep],
    earlier_step: FakeLifecycleStep,
    later_step: FakeLifecycleStep,
) {
    let earlier_index =
        lifecycle_step_index(steps, earlier_step).expect("earlier lifecycle step must be recorded");
    let later_index =
        lifecycle_step_index(steps, later_step).expect("later lifecycle step must be recorded");
    assert!(
        earlier_index < later_index,
        "{earlier_step:?} must happen before {later_step:?}, actual steps: {steps:?}"
    );
}

fn lifecycle_step_index(
    steps: &[FakeLifecycleStep],
    wanted_step: FakeLifecycleStep,
) -> Option<usize> {
    steps.iter().position(|step| *step == wanted_step)
}

fn lifecycle_step_last_index(
    steps: &[FakeLifecycleStep],
    wanted_step: FakeLifecycleStep,
) -> Option<usize> {
    steps.iter().rposition(|step| *step == wanted_step)
}
