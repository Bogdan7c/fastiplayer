use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use frame_server_core::{PlaybackGeneration, ScrubGeneration};
use media_core::{
    DemuxReadEvent, DemuxSeekRequest, DemuxSeekResult, Demuxer, MediaDemuxError, Packet, TimeBase,
    TrackId, TrackInfo,
};

use crate::timeline_hover_source::{
    TimelineHoverOpenFailedSourceKind, TimelineHoverOpenedSource, TimelineHoverSourceIdentity,
};

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

struct FakeHoverDemuxer {
    tracks: Vec<TrackInfo>,
    decode_point_before_requests: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    ordinary_seek_count: Arc<Mutex<usize>>,
    seek_outcome: FakeHoverDemuxerSeekOutcome,
}

#[derive(Clone, Copy)]
enum FakeHoverDemuxerSeekOutcome {
    Resolved(DemuxSeekResult),
    UnsupportedDecodePointBefore,
}

impl FakeHoverDemuxer {
    fn resolved(
        seek_result: DemuxSeekResult,
    ) -> (Self, Arc<Mutex<Vec<DemuxSeekRequest>>>, Arc<Mutex<usize>>) {
        Self::new(FakeHoverDemuxerSeekOutcome::Resolved(seek_result))
    }

    fn unsupported_decode_point_before()
    -> (Self, Arc<Mutex<Vec<DemuxSeekRequest>>>, Arc<Mutex<usize>>) {
        Self::new(FakeHoverDemuxerSeekOutcome::UnsupportedDecodePointBefore)
    }

    fn new(
        seek_outcome: FakeHoverDemuxerSeekOutcome,
    ) -> (Self, Arc<Mutex<Vec<DemuxSeekRequest>>>, Arc<Mutex<usize>>) {
        let decode_point_before_requests = Arc::new(Mutex::new(Vec::new()));
        let ordinary_seek_count = Arc::new(Mutex::new(0));
        (
            Self {
                tracks: Vec::new(),
                decode_point_before_requests: Arc::clone(&decode_point_before_requests),
                ordinary_seek_count: Arc::clone(&ordinary_seek_count),
                seek_outcome,
            },
            decode_point_before_requests,
            ordinary_seek_count,
        )
    }
}

impl Demuxer for FakeHoverDemuxer {
    fn tracks(&self) -> &[TrackInfo] {
        &self.tracks
    }

    fn duration(&self) -> Option<Duration> {
        Some(Duration::from_secs(30))
    }

    fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
        Ok(None)
    }

    fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
        Ok(DemuxReadEvent::EndOfStream)
    }

    fn seek(&mut self, timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
        *self
            .ordinary_seek_count
            .lock()
            .expect("ordinary seek counter lock must be available") += 1;
        Ok(DemuxSeekResult {
            requested_position: media_core::MediaTime::from_duration(timestamp),
            actual_position: media_core::MediaTime::from_duration(timestamp),
            actual_track_timestamp: None,
        })
    }

    fn seek_with_request(&mut self, request: DemuxSeekRequest) -> anyhow::Result<DemuxSeekResult> {
        self.decode_point_before_requests
            .lock()
            .expect("decode-point-before request log lock must be available")
            .push(request);
        match self.seek_outcome {
            FakeHoverDemuxerSeekOutcome::Resolved(seek_result) => Ok(seek_result),
            FakeHoverDemuxerSeekOutcome::UnsupportedDecodePointBefore => {
                Err(MediaDemuxError::UnsupportedSeekMode { mode: request.mode }.into())
            }
        }
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

fn unresolved_active_target(target_millis: i64) -> TimelineHoverPrepareTarget {
    TimelineHoverPrepareTarget::unresolved(
        context(),
        timestamp(target_millis),
        TimelineHoverFrameBucket::new(target_millis),
        TimelineHoverPreparePlaybackMode::ActivePlayback,
    )
}

#[test]
fn unresolved_target_does_not_supply_fake_dependency_span() {
    let active_target = unresolved_active_target(10_000);

    assert!(active_target.dependency_span().is_none());
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
fn resolved_span_clears_stale_pending_unresolved_work() {
    let mut controller = TimelineHoverPrepareController::new(FakeExecutor::default());
    controller.pending_unresolved_target = Some(InFlightTimelineHoverPrepareTarget {
        span_id: TimelineHoverPrepareSpanId(77),
        latest_target: unresolved_active_target(10_000),
    });

    let outcome = controller.prepare_hover_target(target(10_000, 9_000, 10_100));

    assert!(controller.pending_unresolved_target.is_none());
    let cancellation = controller
        .cancel_active_span(TimelineHoverPrepareCancellationReason::TimelineLeft)
        .expect("resolved active span must be cancellable");

    assert_eq!(cancellation.span_id, outcome.span_id);
    assert!(controller.pending_unresolved_target.is_none());
    assert!(
        controller
            .cancel_active_span(TimelineHoverPrepareCancellationReason::TimelineLeft)
            .is_none()
    );
    assert_eq!(controller.executor().cancellations.len(), 1);
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
fn active_playback_without_source_context_degrades_to_typed_missing_noop() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceMissing { .. },
        }
    ));
    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::NoPreparedHit
    );
}

#[test]
fn active_playback_independent_demuxer_resolves_span_without_playback_seek() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    let decode_safe_start = timestamp(9_000);
    let (demuxer, decode_point_before_requests, ordinary_seek_count) =
        FakeHoverDemuxer::resolved(DemuxSeekResult {
            requested_position: timestamp(10_000).to_media_time(),
            actual_position: decode_safe_start.to_media_time(),
            actual_track_timestamp: Some(decode_safe_start),
        });
    controller.executor.active_hover_source =
        Some(TimelineHoverOpenedSource::from_demuxer(Box::new(demuxer)));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert_eq!(
        *decode_point_before_requests
            .lock()
            .expect("request log lock must be available"),
        vec![DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10
        ))]
    );
    assert_eq!(
        *ordinary_seek_count
            .lock()
            .expect("ordinary seek counter lock must be available"),
        0
    );
    assert_eq!(
        outcome.transition,
        TimelineHoverPrepareControllerTransition::Started
    );
    // Без hover decode session (backend build не подключал её) resolved span
    // деградирует typed-ом: executor недоступен, span остаётся pending.
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
    let active_span = controller
        .active_span
        .expect("resolved active playback target must install an active span");
    assert_eq!(active_span.span.decode_safe_start_pts, decode_safe_start);
    assert_eq!(active_span.span.drain_until_pts, timestamp(10_000));

    let forward_outcome = controller.prepare_hover_target(unresolved_active_target(10_500));

    assert_eq!(
        *decode_point_before_requests
            .lock()
            .expect("request log lock must be available"),
        vec![DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10
        ))]
    );
    assert_eq!(
        forward_outcome.transition,
        TimelineHoverPrepareControllerTransition::ExtendedForward
    );
    assert_eq!(
        controller
            .active_span
            .expect("forward target must keep active span")
            .span
            .drain_until_pts,
        timestamp(10_500)
    );
}

#[test]
fn active_playback_dependency_span_resolver_failure_is_typed_noop() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    let (demuxer, decode_point_before_requests, ordinary_seek_count) =
        FakeHoverDemuxer::unsupported_decode_point_before();
    controller.executor.active_hover_source =
        Some(TimelineHoverOpenedSource::from_demuxer(Box::new(demuxer)));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert_eq!(
        *decode_point_before_requests
            .lock()
            .expect("request log lock must be available"),
        vec![DemuxSeekRequest::decode_point_before(Duration::from_secs(
            10
        ))]
    );
    assert_eq!(
        *ordinary_seek_count
            .lock()
            .expect("ordinary seek counter lock must be available"),
        0
    );
    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason:
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackDependencySpanSeekUnsupported,
        }
    ));
    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::NoPreparedHit
    );
    assert!(controller.active_span.is_none());
}

#[test]
fn active_playback_network_source_ready_does_not_seek_on_ui_thread() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    controller.set_hover_source(TimelineHoverSourceIdentity::DirectMediaUrl(
        "https://example.invalid/video.mp4".to_string(),
    ));
    let (demuxer, decode_point_before_requests, ordinary_seek_count) =
        FakeHoverDemuxer::resolved(DemuxSeekResult {
            requested_position: timestamp(10_000).to_media_time(),
            actual_position: timestamp(9_000).to_media_time(),
            actual_track_timestamp: Some(timestamp(9_000)),
        });
    controller.executor.active_hover_source =
        Some(TimelineHoverOpenedSource::from_demuxer(Box::new(demuxer)));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(
        decode_point_before_requests
            .lock()
            .expect("request log lock must be available")
            .is_empty()
    );
    assert_eq!(
        *ordinary_seek_count
            .lock()
            .expect("ordinary seek counter lock must be available"),
        0
    );
    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason:
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceReadyDecodeNotWired { .. },
        }
    ));
    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::NoPreparedHit
    );
}

#[test]
fn active_playback_network_source_opens_as_background_latest_only_work() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    controller.set_hover_source(TimelineHoverSourceIdentity::DirectMediaUrl(
        "not-a-valid-url".to_string(),
    ));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. },
        }
    ));
    assert_eq!(
        outcome.completion_outcome,
        TimelineHoverPrepareCompletionOutcome::NoPreparedHit
    );

    let mut retry_outcome = None;
    for _attempt in 0..20 {
        thread::sleep(Duration::from_millis(5));
        let next_outcome = controller.prepare_hover_target(active_target);
        if !matches!(
            next_outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::NoOp {
                reason:
                    TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. },
            }
        ) {
            retry_outcome = Some(next_outcome);
            break;
        }
    }
    let retry_outcome = retry_outcome.expect("background direct open must finish");

    assert!(matches!(
        retry_outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceOpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::DirectMediaUrl,
                ..
            },
        }
    ));

    let held_failure = controller.prepare_hover_target(active_target);
    assert!(matches!(
        held_failure.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason:
                TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceFailedNoRetry { .. },
        }
    ));
}

#[test]
fn network_hover_throttle_live_update_preserves_source_context_and_inflight_job() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    controller.set_hover_source(TimelineHoverSourceIdentity::DirectMediaUrl(
        "not-a-valid-url".to_string(),
    ));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. },
        }
    ));
    let before = controller.executor.network_open_diagnostics_snapshot();
    assert_eq!(before.in_flight_count, 1);

    controller.update_network_hover_prepare_throttle(Duration::ZERO);

    let after = controller.executor.network_open_diagnostics_snapshot();
    assert_eq!(after.source_generation, before.source_generation);
    assert_eq!(after.in_flight_count, 1);
    assert_eq!(after.inter_start_throttle, Duration::ZERO);
}

#[test]
fn cancelling_active_network_hover_stale_marks_pending_open() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    controller.set_hover_source(TimelineHoverSourceIdentity::DirectMediaUrl(
        "not-a-valid-url".to_string(),
    ));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackNetworkSourceOpening { .. },
        }
    ));
    assert!(controller.executor.network_open_controller.has_active_job());

    controller
        .cancel_active_span(TimelineHoverPrepareCancellationReason::TimelineLeft)
        .expect("network hover start creates cancellable pending work");

    assert!(
        controller.executor.network_open_controller.has_active_job(),
        "same-source remote open stays tracked until its stale result is drained"
    );
}

#[test]
fn active_playback_hover_source_open_failure_is_not_a_playback_reset() {
    let mut controller = TimelineHoverPrepareController::new(AppTimelineHoverPrepareExecutor::new(
        PlayerTimelineHoverPrepareHandoff::default(),
    ));
    controller.set_hover_source(TimelineHoverSourceIdentity::LocalFile(PathBuf::from(
        "/tmp/rustiplayer-missing-hover-source-for-controller.wav",
    )));
    let active_target = unresolved_active_target(10_000);

    let outcome = controller.prepare_hover_target(active_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackSourceOpenFailed {
                source_kind: TimelineHoverOpenFailedSourceKind::LocalFile,
                ..
            },
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
    let live_target = TimelineHoverPrepareTarget::unresolved(
        context(),
        timestamp(10_000),
        TimelineHoverFrameBucket::new(10_000),
        TimelineHoverPreparePlaybackMode::LiveScrubActive,
    );

    let outcome = controller.prepare_hover_target(live_target);

    assert!(matches!(
        outcome.executor_outcome,
        TimelineHoverPrepareExecutorOutcome::NoOp {
            reason: TimelineHoverPrepareExecutorNoOpReason::LiveScrubSuspended,
        }
    ));
    assert!(controller.active_span.is_none());
}

// ---------------------------------------------------------------------------
// Active-playback hover decode execution (feed/drain driver + insert boundary)
// ---------------------------------------------------------------------------

mod decode_execution {
    use std::num::NonZeroUsize;
    use std::time::Duration;

    use codec_core::{VideoCodec, VideoColorMetadata, VideoDisplayOrientation};
    use frame_server_core::{
        HoverBudgetResolutionSource, HoverBudgetResourceClass, HoverResolvedBudget,
        HoverResolvedBudgetResource,
    };
    use media_core::TrackKind;
    use player_core::PlayerHoverStreamDecodeContext;
    use video_backend_api::{
        PresentFrameResourceProvider, PresentFrameResourceProviderHandle,
        PresentFrameResourceProviderLookup,
    };
    use video_core::{
        DecodePacket, DecodeSendError, DecodeThreadError, DecodedFrame, DecoderResourceSnapshot,
        FrameResourceHandle, VideoDecoderEndOfStreamDrainResult, VideoDecoderEndOfStreamDrainState,
        VideoDecoderThreadHandle, VideoFrameDiagnostics, VideoStreamConfigResult,
        VideoStreamDecodeConfig,
    };
    use video_ffmpeg::software_hover::FfmpegSoftwareHoverContext;
    use video_ffmpeg::{FfmpegSoftwareHoverAdmission, FfmpegSoftwareHoverOwner};
    use video_frame_contract::VideoFrameContract;

    use super::*;
    use crate::timeline_hover_decode::TimelineHoverDecodeSession;

    #[derive(Default)]
    struct FakeHoverDecoderShared {
        sent_packets: Vec<DecodePacket>,
        flush_count: usize,
        configured: Vec<VideoStreamDecodeConfig>,
        scripted_frames: VecDeque<DecodedFrame>,
        frames_ready_after_packets: usize,
        released_handles: Vec<u64>,
        backpressure_after_packets: Option<usize>,
        eos_drain_generation: Option<u64>,
    }

    struct FakeHoverDecoderThread {
        shared: Arc<Mutex<FakeHoverDecoderShared>>,
        provider: PresentFrameResourceProviderHandle,
    }

    impl FakeHoverDecoderThread {
        fn new(shared: Arc<Mutex<FakeHoverDecoderShared>>) -> Self {
            let provider = PresentFrameResourceProviderHandle::new(FakeHoverResourceProvider);
            Self { shared, provider }
        }
    }

    impl VideoDecoderThreadHandle for FakeHoverDecoderThread {
        type ResourceProvider = PresentFrameResourceProviderHandle;

        fn backend_name(&self) -> &'static str {
            "fake-hover-decoder"
        }

        fn send_packet(&self, packet: DecodePacket) -> Result<(), DecodeSendError> {
            let mut shared = self.shared.lock().expect("fake decoder lock");
            if let Some(limit) = shared.backpressure_after_packets
                && shared.sent_packets.len() >= limit
            {
                return Err(DecodeSendError::Backpressure(
                    video_core::DecodeBackpressureReason::PacketQueueFull {
                        queued_packets: shared.sent_packets.len(),
                        capacity: limit,
                    },
                ));
            }
            shared.sent_packets.push(packet);
            Ok(())
        }

        fn configure_stream(&self, config: VideoStreamDecodeConfig) -> VideoStreamConfigResult {
            self.shared
                .lock()
                .expect("fake decoder lock")
                .configured
                .push(config);
            VideoStreamConfigResult::Configured
        }

        fn begin_end_of_stream_drain(&self, generation: u64) -> VideoDecoderEndOfStreamDrainResult {
            self.shared
                .lock()
                .expect("fake decoder lock")
                .eos_drain_generation = Some(generation);
            VideoDecoderEndOfStreamDrainResult::Started(
                VideoDecoderEndOfStreamDrainState::Drained { generation },
            )
        }

        fn end_of_stream_drain_state(&self) -> VideoDecoderEndOfStreamDrainState {
            match self
                .shared
                .lock()
                .expect("fake decoder lock")
                .eos_drain_generation
            {
                Some(generation) => VideoDecoderEndOfStreamDrainState::Drained { generation },
                None => VideoDecoderEndOfStreamDrainState::Idle,
            }
        }

        fn release_frame(&self, handle: FrameResourceHandle) {
            self.shared
                .lock()
                .expect("fake decoder lock")
                .released_handles
                .push(handle.0);
        }

        fn try_recv_frame(&self) -> Option<DecodedFrame> {
            let mut shared = self.shared.lock().expect("fake decoder lock");
            if shared.sent_packets.len() < shared.frames_ready_after_packets {
                return None;
            }
            shared.scripted_frames.pop_front()
        }

        fn try_recv_diagnostic_event(&self) -> Option<video_core::VideoDecoderDiagnosticEvent> {
            None
        }

        fn try_recv_error(&self) -> Option<DecodeThreadError> {
            None
        }

        fn flush(&self) -> anyhow::Result<()> {
            self.shared.lock().expect("fake decoder lock").flush_count += 1;
            Ok(())
        }

        fn resource_provider(&self) -> Self::ResourceProvider {
            self.provider.clone()
        }

        fn decoder_resource_snapshot(&self) -> Option<DecoderResourceSnapshot> {
            None
        }

        fn packet_queue_depth(&self) -> usize {
            0
        }

        fn drain_completed_packet_count(&self) -> usize {
            self.shared
                .lock()
                .expect("fake decoder lock")
                .sent_packets
                .len()
        }
    }

    struct FakeHoverResourceProvider;

    impl PresentFrameResourceProvider for FakeHoverResourceProvider {
        fn resource_lookup(
            &self,
            _handle: FrameResourceHandle,
        ) -> PresentFrameResourceProviderLookup {
            PresentFrameResourceProviderLookup::Ready {
                resource_pool_lock_wait: Duration::ZERO,
            }
        }

        fn release_frame(&self, _handle: FrameResourceHandle) {}
    }

    /// Demuxer, отдающий заранее заданный поток packet/EOS событий после seek-а.
    struct ScriptedPacketDemuxer {
        tracks: Vec<TrackInfo>,
        events: VecDeque<DemuxReadEvent>,
        seek_requests: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    }

    impl ScriptedPacketDemuxer {
        fn new(events: Vec<DemuxReadEvent>) -> (Self, Arc<Mutex<Vec<DemuxSeekRequest>>>) {
            let seek_requests = Arc::new(Mutex::new(Vec::new()));
            (
                Self {
                    tracks: Vec::new(),
                    events: events.into(),
                    seek_requests: Arc::clone(&seek_requests),
                },
                seek_requests,
            )
        }
    }

    impl Demuxer for ScriptedPacketDemuxer {
        fn tracks(&self) -> &[TrackInfo] {
            &self.tracks
        }

        fn duration(&self) -> Option<Duration> {
            Some(Duration::from_secs(30))
        }

        fn next_packet(&mut self) -> anyhow::Result<Option<Packet>> {
            match self.next_event()? {
                DemuxReadEvent::Packet(packet) => Ok(Some(packet)),
                _ => Ok(None),
            }
        }

        fn next_event(&mut self) -> anyhow::Result<DemuxReadEvent> {
            Ok(self
                .events
                .pop_front()
                .unwrap_or(DemuxReadEvent::EndOfStream))
        }

        fn seek(&mut self, _timestamp: Duration) -> anyhow::Result<DemuxSeekResult> {
            panic!("hover decode must not use ordinary accurate seek");
        }

        fn seek_with_request(
            &mut self,
            request: DemuxSeekRequest,
        ) -> anyhow::Result<DemuxSeekResult> {
            self.seek_requests
                .lock()
                .expect("seek request log lock")
                .push(request);
            Ok(DemuxSeekResult {
                requested_position: request.timestamp.into(),
                actual_position: timestamp(9_400).to_media_time(),
                actual_track_timestamp: Some(timestamp(9_400)),
            })
        }
    }

    fn video_packet(pts_millis: u64, keyframe: bool) -> DemuxReadEvent {
        DemuxReadEvent::Packet(Packet::new(
            TrackId::new(1),
            TrackKind::Video,
            Duration::from_millis(pts_millis),
            None,
            keyframe,
            vec![0u8; 4].into(),
        ))
    }

    fn decoded_frame(pts_millis: u64, generation: u64, resource_handle: u64) -> DecodedFrame {
        DecodedFrame {
            generation,
            pts: Duration::from_millis(pts_millis),
            frame_contract: VideoFrameContract::host_yuv420_planar8(),
            width: 640,
            height: 360,
            render_width: 640,
            render_height: 360,
            display_orientation: VideoDisplayOrientation::Identity,
            color: VideoColorMetadata::sdr_bt709_limited(),
            resource_handle: FrameResourceHandle(resource_handle),
            diagnostics: VideoFrameDiagnostics::default(),
        }
    }

    fn hover_stream_context() -> PlayerHoverStreamDecodeContext {
        PlayerHoverStreamDecodeContext {
            stream_config: VideoStreamDecodeConfig {
                track_id: TrackId::new(1),
                codec: VideoCodec::H264,
                profile: None,
                bit_depth: None,
                chroma: None,
                coded_width: Some(640),
                coded_height: Some(360),
                display_orientation: VideoDisplayOrientation::Identity,
                frame_contract: VideoFrameContract::host_yuv420_planar8(),
                codec_private: None,
                packetization: None,
            },
            resolved_color: None,
        }
    }

    fn nz(value: usize) -> NonZeroUsize {
        NonZeroUsize::new(value).expect("non-zero test value")
    }

    fn decode_session_with_shared(
        shared: Arc<Mutex<FakeHoverDecoderShared>>,
    ) -> TimelineHoverDecodeSession {
        let owner = FfmpegSoftwareHoverOwner::new(
            FfmpegSoftwareHoverContext::from_playback_decoder_config(
                video_core::VideoDecoderThreadConfig::default(),
            ),
        );
        let resolved_budget = HoverResolvedBudget::new(vec![
            HoverResolvedBudgetResource::new(
                HoverBudgetResourceClass::SoftwareFramePoolFrames,
                nz(2),
                HoverBudgetResolutionSource::BackendMinimumAuto,
            ),
            HoverResolvedBudgetResource::new(
                HoverBudgetResourceClass::SoftwareThreadCount,
                nz(1),
                HoverBudgetResolutionSource::BackendMinimumAuto,
            ),
        ]);
        let reservation = match owner.admit_hover_reservation(&resolved_budget) {
            FfmpegSoftwareHoverAdmission::Admitted(reservation) => reservation,
            FfmpegSoftwareHoverAdmission::Rejected(report) => {
                panic!("test hover reservation must be admitted: {report:?}")
            }
        };
        let thread = FakeHoverDecoderThread::new(shared);
        let provider = thread.resource_provider();
        TimelineHoverDecodeSession::new(Box::new(thread), provider, reservation, resolved_budget)
    }

    struct DecodeHarness {
        controller: TimelineHoverPrepareController<AppTimelineHoverPrepareExecutor>,
        handoff: PlayerTimelineHoverPrepareHandoff,
        shared: Arc<Mutex<FakeHoverDecoderShared>>,
        seek_requests: Arc<Mutex<Vec<DemuxSeekRequest>>>,
    }

    fn decode_harness(events: Vec<DemuxReadEvent>) -> DecodeHarness {
        let handoff = PlayerTimelineHoverPrepareHandoff::default();
        handoff.publish_hover_stream_decode_context(hover_stream_context());
        let mut controller = TimelineHoverPrepareController::new(
            AppTimelineHoverPrepareExecutor::new(handoff.clone()),
        );
        let shared = Arc::new(Mutex::new(FakeHoverDecoderShared::default()));
        controller.executor.decode_session = Some(decode_session_with_shared(Arc::clone(&shared)));
        let (demuxer, seek_requests) = ScriptedPacketDemuxer::new(events);
        controller.executor.active_hover_source =
            Some(TimelineHoverOpenedSource::from_demuxer(Box::new(demuxer)));
        DecodeHarness {
            controller,
            handoff,
            shared,
            seek_requests,
        }
    }

    #[test]
    fn decode_prepares_first_target_or_after_frame_and_releases_pre_target() {
        let mut harness = decode_harness(vec![
            video_packet(9_400, true),
            video_packet(9_600, false),
            video_packet(10_020, false),
        ]);
        {
            let mut shared = harness.shared.lock().expect("fake decoder lock");
            shared.frames_ready_after_packets = 3;
            shared.scripted_frames = VecDeque::from(vec![
                decoded_frame(9_600, 1, 7),
                decoded_frame(10_020, 1, 8),
            ]);
        }
        let active_target = unresolved_active_target(10_000);

        let outcome = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::PreparedHit {
                actual_pts,
                exactness: TimelineHoverPreparePreparedHitExactness::ExactTargetOrAfter,
                ..
            } if actual_pts == timestamp(10_020)
        ));
        assert!(matches!(
            outcome.completion_outcome,
            TimelineHoverPrepareCompletionOutcome::AcceptedExactPreparedHit { actual_pts, .. }
                if actual_pts == timestamp(10_020)
        ));

        let shared = harness.shared.lock().expect("fake decoder lock");
        assert_eq!(shared.flush_count, 1);
        assert_eq!(shared.configured.len(), 1);
        assert_eq!(shared.sent_packets.len(), 3);
        // Pre-target кадр released обратно в decoder pool, не вставлен.
        assert_eq!(shared.released_handles, vec![7]);
        drop(shared);

        // Первый target-or-after кадр реально вставлен в shared working set.
        match harness
            .handoff
            .borrow_prepared_frame(active_target.lookup_request())
        {
            PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(borrowed) => {
                assert_eq!(borrowed.timing().actual_pts(), timestamp(10_020));
            }
            other_outcome => panic!(
                "prepared frame must be borrowable: miss/timing unexpected ({})",
                match other_outcome {
                    PlayerTimelineHoverPrepareBorrowOutcome::Miss(_) => "miss",
                    PlayerTimelineHoverPrepareBorrowOutcome::TimingRejected(_) => "timing",
                    PlayerTimelineHoverPrepareBorrowOutcome::Borrowed(_) => unreachable!(),
                }
            ),
        }

        // Повторный prepare того же target-а — working set hit без нового декода.
        let repeat_outcome = harness.controller.prepare_hover_target(active_target);
        assert!(matches!(
            repeat_outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::WorkingSetHit { actual_pts }
                if actual_pts == timestamp(10_020)
        ));
        assert_eq!(
            harness
                .shared
                .lock()
                .expect("fake decoder lock")
                .flush_count,
            1,
            "working-set hit must not restart decode span"
        );
    }

    #[test]
    fn decode_budget_exhausts_and_continues_without_reseek() {
        let mut events: Vec<DemuxReadEvent> = (0..40)
            .map(|index| video_packet(9_400 + index * 20, index == 0))
            .collect();
        events.push(video_packet(10_200, false));
        let mut harness = decode_harness(events);
        {
            let mut shared = harness.shared.lock().expect("fake decoder lock");
            shared.frames_ready_after_packets = 41;
            shared.scripted_frames = VecDeque::from(vec![decoded_frame(10_040, 1, 3)]);
        }
        let active_target = unresolved_active_target(10_000);

        let first_pass = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            first_pass.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
                reason: TimelineHoverPrepareIncompleteReason::DecodeBudgetExhausted,
                diagnostics,
            } if diagnostics.decoded_packets() == 32
        ));
        assert_eq!(
            harness.seek_requests.lock().expect("seek log lock").len(),
            1
        );

        let second_pass = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            second_pass.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::PreparedHit { actual_pts, .. }
                if actual_pts == timestamp(10_040)
        ));
        let shared = harness.shared.lock().expect("fake decoder lock");
        assert_eq!(
            shared.flush_count, 1,
            "same-span continuation must not flush"
        );
        drop(shared);
        assert_eq!(
            harness.seek_requests.lock().expect("seek log lock").len(),
            1,
            "same-span continuation must not reseek demuxer"
        );
    }

    #[test]
    fn decode_backpressure_keeps_pending_packet_without_loss() {
        let mut harness =
            decode_harness(vec![video_packet(9_400, true), video_packet(9_600, false)]);
        {
            let mut shared = harness.shared.lock().expect("fake decoder lock");
            shared.backpressure_after_packets = Some(1);
            shared.frames_ready_after_packets = 2;
            shared.scripted_frames = VecDeque::from(vec![decoded_frame(10_020, 1, 5)]);
        }
        let active_target = unresolved_active_target(10_000);

        let first_pass = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            first_pass.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::Pressure {
                pressure: TimelineHoverPreparePressure::DecoderBackpressure,
            }
        ));

        harness
            .shared
            .lock()
            .expect("fake decoder lock")
            .backpressure_after_packets = None;

        let second_pass = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            second_pass.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::PreparedHit { .. }
        ));
        let shared = harness.shared.lock().expect("fake decoder lock");
        let sent_pts: Vec<Duration> = shared
            .sent_packets
            .iter()
            .map(|packet| packet.pts)
            .collect();
        assert_eq!(
            sent_pts,
            vec![Duration::from_millis(9_400), Duration::from_millis(9_600)],
            "backpressured packet must be delivered exactly once without loss"
        );
    }

    #[test]
    fn decode_end_of_stream_before_target_is_typed_incomplete() {
        let mut harness = decode_harness(vec![video_packet(9_400, true)]);
        let active_target = unresolved_active_target(10_000);

        let outcome = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
                reason: TimelineHoverPrepareIncompleteReason::EndOfStreamBeforeTarget,
                ..
            }
        ));
        assert!(matches!(
            harness
                .handoff
                .borrow_prepared_frame(active_target.lookup_request()),
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(_)
        ));
    }

    #[test]
    fn stale_generation_frames_are_released_without_insert() {
        let mut harness = decode_harness(vec![video_packet(9_400, true)]);
        {
            let mut shared = harness.shared.lock().expect("fake decoder lock");
            // generation 99 не совпадает с session generation 1 → stale release.
            shared.scripted_frames = VecDeque::from(vec![decoded_frame(10_020, 99, 11)]);
        }
        let active_target = unresolved_active_target(10_000);

        let outcome = harness.controller.prepare_hover_target(active_target);

        assert!(matches!(
            outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::IncompleteSpan {
                reason: TimelineHoverPrepareIncompleteReason::EndOfStreamBeforeTarget,
                ..
            }
        ));
        let shared = harness.shared.lock().expect("fake decoder lock");
        assert_eq!(shared.released_handles, vec![11]);
        drop(shared);
        assert!(matches!(
            harness
                .handoff
                .borrow_prepared_frame(active_target.lookup_request()),
            PlayerTimelineHoverPrepareBorrowOutcome::Miss(_)
        ));
    }

    #[test]
    fn missing_stream_context_degrades_typed_without_decode() {
        let harness_handoff = PlayerTimelineHoverPrepareHandoff::default();
        let mut controller = TimelineHoverPrepareController::new(
            AppTimelineHoverPrepareExecutor::new(harness_handoff.clone()),
        );
        let shared = Arc::new(Mutex::new(FakeHoverDecoderShared::default()));
        controller.executor.decode_session = Some(decode_session_with_shared(Arc::clone(&shared)));
        let (demuxer, _seek_requests) = ScriptedPacketDemuxer::new(vec![video_packet(9_400, true)]);
        controller.executor.active_hover_source =
            Some(TimelineHoverOpenedSource::from_demuxer(Box::new(demuxer)));
        let active_target = unresolved_active_target(10_000);

        let outcome = controller.prepare_hover_target(active_target);

        assert!(matches!(
            outcome.executor_outcome,
            TimelineHoverPrepareExecutorOutcome::NoOp {
                reason: TimelineHoverPrepareExecutorNoOpReason::ActivePlaybackExecutorUnavailable { .. },
            }
        ));
        assert_eq!(
            shared.lock().expect("fake decoder lock").sent_packets.len(),
            0,
            "decode must not start without published stream config"
        );
    }
}
