use std::collections::VecDeque;

use frame_server_core::{PlaybackGeneration, ScrubGeneration};
use media_core::{TimeBase, TrackId};

use super::*;

#[derive(Default)]
struct FakeExecutor {
    requests: Vec<TimelineHoverPrepareExecutorRequest>,
    cancellations: Vec<TimelineHoverPrepareCancelRequest>,
    queued_outcomes: VecDeque<TimelineHoverPrepareExecutorOutcome>,
    ordinary_seek_commands_sent: usize,
}

impl FakeExecutor {
    fn push_outcome(&mut self, outcome: TimelineHoverPrepareExecutorOutcome) {
        self.queued_outcomes.push_back(outcome);
    }
}

impl TimelineHoverPrepareExecutor for FakeExecutor {
    fn prepare_exact_dependency_span(
        &mut self,
        request: TimelineHoverPrepareExecutorRequest,
    ) -> TimelineHoverPrepareExecutorOutcome {
        self.requests.push(request);
        self.queued_outcomes
            .pop_front()
            .unwrap_or(TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::PausedStoppedExecutorNotWired {
                    lookup_miss: TimelineHoverPrepareLookupMissReason::NoEntryForKey,
                },
            })
    }

    fn cancel_dependency_span(&mut self, request: TimelineHoverPrepareCancelRequest) {
        self.cancellations.push(request);
    }
}

fn timestamp(millis: i64) -> TrackTimestamp {
    TrackTimestamp::new(
        TrackId::new(1),
        millis,
        TimeBase::new(1, 1_000).expect("valid millisecond timebase"),
    )
}

fn context() -> TimelineHoverPrepareTargetContext {
    TimelineHoverPrepareTargetContext::new(
        SourceRevision::new(1),
        BackendRevision::new(2),
        ScrubTrackSelection::video_only(TrackId::new(1)),
        ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
        FrameExactnessPolicy::TargetOrAfter,
    )
}

fn target(
    target_millis: i64,
    decode_safe_start_millis: i64,
    drain_until_millis: i64,
) -> TimelineHoverPrepareTarget {
    TimelineHoverPrepareTarget::new(
        context(),
        timestamp(target_millis),
        TimelineHoverFrameBucket::new(target_millis),
        timestamp(decode_safe_start_millis),
        timestamp(drain_until_millis),
        0,
        TimelineHoverPreparePlaybackMode::PausedOrStopped,
    )
    .expect("test target must build a valid dependency span")
}

fn target_with_context(source_revision: u64, target_millis: i64) -> TimelineHoverPrepareTarget {
    let target_context = TimelineHoverPrepareTargetContext::new(
        SourceRevision::new(source_revision),
        BackendRevision::new(2),
        ScrubTrackSelection::video_only(TrackId::new(1)),
        ScrubGenerationToken::new(PlaybackGeneration::new(3), ScrubGeneration::new(4)),
        FrameExactnessPolicy::TargetOrAfter,
    );
    TimelineHoverPrepareTarget::new(
        target_context,
        timestamp(target_millis),
        TimelineHoverFrameBucket::new(target_millis),
        timestamp(target_millis - 1_000),
        timestamp(target_millis + 100),
        0,
        TimelineHoverPreparePlaybackMode::PausedOrStopped,
    )
    .expect("test target must build a valid dependency span")
}

#[test]
fn synthetic_target_drives_executor_without_ordinary_seek() {
    let mut fake_executor = FakeExecutor::default();
    fake_executor.push_outcome(TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
        reason: TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted,
        diagnostics: TimelineHoverPrepareSpanDiagnostics::new(12, 3, 1),
    });
    let mut controller = TimelineHoverPrepareController::new(fake_executor);

    let outcome = controller.prepare_hover_target(target(10_000, 9_000, 10_100));

    assert_eq!(
        outcome.transition,
        TimelineHoverPrepareControllerTransition::Started
    );
    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
            reason: TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted,
            diagnostics: TimelineHoverPrepareSpanDiagnostics {
                decoded_packets: 12,
                decoded_frames: 3,
                post_target_reorder_drain_frames: 1,
            },
        }
    ));
    assert_eq!(controller.executor().ordinary_seek_commands_sent, 0);
    assert_eq!(controller.executor().requests.len(), 1);
}

#[test]
fn same_span_retarget_updates_latest_target_without_restart() {
    let mut controller = TimelineHoverPrepareController::new(FakeExecutor::default());

    let first = controller.prepare_hover_target(target(10_000, 9_000, 10_300));
    let second = controller.prepare_hover_target(target(10_100, 9_000, 10_300));

    assert_eq!(first.span_id, second.span_id);
    assert_eq!(
        second.transition,
        TimelineHoverPrepareControllerTransition::RetargetedWithinSpan
    );
    assert_eq!(
        controller.executor().requests[1].transition,
        TimelineHoverPrepareExecutorTransition::RetargetWithinSpan
    );
    assert!(controller.executor().cancellations.is_empty());
}

#[test]
fn forward_extension_continues_existing_span_without_cancel() {
    let mut controller = TimelineHoverPrepareController::new(FakeExecutor::default());

    let first = controller.prepare_hover_target(target(10_000, 9_000, 10_100));
    let extended = controller.prepare_hover_target(target(10_200, 9_000, 10_500));

    assert_eq!(first.span_id, extended.span_id);
    assert_eq!(
        extended.transition,
        TimelineHoverPrepareControllerTransition::ExtendedForward
    );
    assert_eq!(
        controller.executor().requests[1].transition,
        TimelineHoverPrepareExecutorTransition::ExtendForward
    );
    assert!(controller.executor().cancellations.is_empty());
}

#[test]
fn earlier_decode_safe_start_supersedes_with_typed_cancellation() {
    let mut controller = TimelineHoverPrepareController::new(FakeExecutor::default());

    let first = controller.prepare_hover_target(target(10_000, 9_000, 10_100));
    let replacement = controller.prepare_hover_target(target(9_800, 8_000, 10_100));

    assert_ne!(first.span_id, replacement.span_id);
    assert!(matches!(
        replacement.transition,
        TimelineHoverPrepareControllerTransition::Superseded {
            cancelled_span_id,
            reason: TimelineHoverPrepareCancellationReason::EarlierDecodeSafeStartRequired,
        } if cancelled_span_id == first.span_id
    ));
    assert_eq!(
        controller.executor().cancellations[0].reason,
        TimelineHoverPrepareCancellationReason::EarlierDecodeSafeStartRequired
    );
}

#[test]
fn incompatible_context_supersedes_with_typed_cancellation() {
    let mut controller = TimelineHoverPrepareController::new(FakeExecutor::default());

    let first = controller.prepare_hover_target(target_with_context(1, 10_000));
    let replacement = controller.prepare_hover_target(target_with_context(9, 10_100));

    assert_ne!(first.span_id, replacement.span_id);
    assert!(matches!(
        replacement.transition,
        TimelineHoverPrepareControllerTransition::Superseded {
            reason: TimelineHoverPrepareCancellationReason::IncompatibleTargetContext,
            ..
        }
    ));
    assert_eq!(
        controller.executor().cancellations[0].reason,
        TimelineHoverPrepareCancellationReason::IncompatibleTargetContext
    );
}

#[test]
fn stale_previous_target_completion_cannot_commit() {
    let mut fake_executor = FakeExecutor::default();
    fake_executor.push_outcome(TimelineHoverPrepareExecutorOutcome::PreparedHit {
        actual_pts: timestamp(10_000),
        exactness: TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
        diagnostics: TimelineHoverPrepareSpanDiagnostics::new(10, 2, 0),
    });
    fake_executor.push_outcome(TimelineHoverPrepareExecutorOutcome::PreparedHit {
        actual_pts: timestamp(10_100),
        exactness: TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
        diagnostics: TimelineHoverPrepareSpanDiagnostics::new(11, 3, 0),
    });
    let mut controller = TimelineHoverPrepareController::new(fake_executor);

    let stale = controller.prepare_hover_target(target(10_000, 9_000, 10_300));
    let latest = controller.prepare_hover_target(target(10_100, 9_000, 10_300));

    assert_eq!(
        stale.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::AcceptedExactPreparedHit {
            span_id: stale.span_id,
            actual_pts: timestamp(10_000),
            diagnostics: TimelineHoverPrepareSpanDiagnostics::new(10, 2, 0),
        }
    );
    assert_eq!(
        latest.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::AcceptedExactPreparedHit {
            span_id: latest.span_id,
            actual_pts: timestamp(10_100),
            diagnostics: TimelineHoverPrepareSpanDiagnostics::new(11, 3, 0),
        }
    );

    let old_target = target(10_000, 9_000, 10_300);
    let late_old_hit = controller.accept_prepared_hit(
        latest.span_id,
        old_target,
        timestamp(10_000),
        TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
        TimelineHoverPrepareSpanDiagnostics::new(12, 4, 0),
    );
    assert_eq!(
        late_old_hit,
        TimelineHoverPrepareCompletionOutcome::RejectedStaleTarget {
            completion_span_id: latest.span_id
        }
    );
}

#[test]
fn approximate_keyframe_hit_is_rejected() {
    let mut fake_executor = FakeExecutor::default();
    fake_executor.push_outcome(TimelineHoverPrepareExecutorOutcome::PreparedHit {
        actual_pts: timestamp(9_000),
        exactness: TimelineHoverPreparePreparedHitExactness::ApproximateKeyframe,
        diagnostics: TimelineHoverPrepareSpanDiagnostics::new(1, 1, 0),
    });
    let mut controller = TimelineHoverPrepareController::new(fake_executor);

    let outcome = controller.prepare_hover_target(target(10_000, 9_000, 10_100));

    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::RejectedApproximate {
            completion_span_id: outcome.span_id,
            actual_pts: timestamp(9_000),
        }
    );
}

#[test]
fn exact_labeled_hit_before_target_is_rejected() {
    let mut fake_executor = FakeExecutor::default();
    fake_executor.push_outcome(TimelineHoverPrepareExecutorOutcome::PreparedHit {
        actual_pts: timestamp(9_999),
        exactness: TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
        diagnostics: TimelineHoverPrepareSpanDiagnostics::new(1, 1, 0),
    });
    let mut controller = TimelineHoverPrepareController::new(fake_executor);

    let outcome = controller.prepare_hover_target(target(10_000, 9_000, 10_100));

    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::RejectedTiming {
            completion_span_id: outcome.span_id,
            actual_pts: timestamp(9_999),
            reason: TimelineHoverPreparePreparedHitTimingRejection::ActualPtsBeforeTarget,
        }
    );
}

#[test]
fn active_playback_without_safe_executor_degrades_to_working_set_only_noop() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    let active_target = TimelineHoverPrepareTarget::new(
        context(),
        timestamp(10_000),
        TimelineHoverFrameBucket::new(10_000),
        timestamp(9_000),
        timestamp(10_100),
        0,
        TimelineHoverPreparePlaybackMode::ActivePlayback,
    )
    .expect("test target must build a valid dependency span");

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable { .. },
        }
    ));
    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::NoPreparedHit
    );
}

#[test]
fn live_scrub_target_degrades_to_noop_without_executor_work() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    let live_target = TimelineHoverPrepareTarget::new(
        context(),
        timestamp(10_000),
        TimelineHoverFrameBucket::new(10_000),
        timestamp(9_000),
        timestamp(10_100),
        0,
        TimelineHoverPreparePlaybackMode::LiveScrubActive,
    )
    .expect("test target must build a valid dependency span");

    let outcome = controller.prepare_hover_target(live_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended,
        }
    ));
}
