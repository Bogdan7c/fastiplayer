use std::time::Duration;

use media_core::{MediaTime, TimeBase, TrackId, TrackTimestamp};

use crate::*;

fn generation_token(playback: u64, scrub: u64) -> ScrubGenerationToken {
    ScrubGenerationToken::new(
        PlaybackGeneration::new(playback),
        ScrubGeneration::new(scrub),
    )
}

fn target_for_tests(track_id: TrackId, millis: u64) -> ScrubTarget {
    let time_base = TimeBase::new(1, 1_000).expect("валидная test timebase");
    ScrubTarget::new(
        MediaTime::from_millis(millis),
        TrackTimestamp::new(track_id, millis as i64, time_base),
    )
}

fn context_for_tests_at(request_kind: ScrubRequestKind, millis: u64) -> ScrubTargetContext {
    let video_track = TrackId::new(7);
    ScrubTargetContext::new(
        SourceRevision::new(10),
        BackendRevision::new(20),
        ScrubTrackSelection::with_audio(video_track, TrackId::new(8)),
        target_for_tests(video_track, millis),
        ScrubExactnessPolicy::TargetOrAfter,
        request_kind,
        generation_token(30, 40),
    )
}

fn prepared_contexts(update: &SchedulerUpdate) -> Vec<ScrubTargetContext> {
    update
        .actions
        .iter()
        .filter_map(|action| match action {
            SchedulerAction::DispatchIntent(ScrubIntent::PrepareTarget(intent)) => {
                Some(intent.context)
            }
            SchedulerAction::DispatchIntent(_) => None,
        })
        .collect()
}

fn cancelled_contexts(update: &SchedulerUpdate) -> Vec<ScrubTargetContext> {
    update
        .actions
        .iter()
        .filter_map(|action| match action {
            SchedulerAction::DispatchIntent(ScrubIntent::Cancel(intent)) => Some(intent.context),
            SchedulerAction::DispatchIntent(_) => None,
        })
        .collect()
}

#[test]
fn scheduler_seek_landing_preempts_lower_priority_live_work() {
    let mut scheduler = FrameScheduler::default();
    let live_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 1_000);
    let seek_context = context_for_tests_at(ScrubRequestKind::SeekLanding, 2_000);

    scheduler.submit_live_scrub_target(live_context);
    let live_start = scheduler.tick(Duration::ZERO);
    assert_eq!(prepared_contexts(&live_start), vec![live_context]);

    scheduler.submit_seek_landing_target(seek_context);
    let preempted = scheduler.tick(Duration::from_millis(1));

    assert_eq!(cancelled_contexts(&preempted), vec![live_context]);
    assert_eq!(prepared_contexts(&preempted), vec![seek_context]);
    assert_eq!(
        preempted.diagnostics,
        vec![SchedulerDiagnostic::ActiveWorkCancelled {
            cancelled_context: live_context,
            replacement_context: Some(seek_context),
            reason: CancelScrubReason::SupersededByNewTarget,
        }]
    );
    assert_eq!(
        scheduler.active_work(),
        Some(SchedulerActiveWork {
            context: seek_context,
            priority: ScrubPriority::UserCommit,
        })
    );
}

#[test]
fn live_scrub_keeps_only_latest_pending_target() {
    let config = FrameServerConfig {
        live_scrub_decode_mode: LiveScrubDecodeMode::EveryDragEvent,
        ..FrameServerConfig::default()
    }
    .validate()
    .expect("valid every-drag scheduler config");
    let mut scheduler = FrameScheduler::new(config);
    let older_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 1_000);
    let latest_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 2_000);

    scheduler.submit_live_scrub_target(older_context);
    scheduler.submit_live_scrub_target(latest_context);

    assert_eq!(scheduler.latest_live_scrub_target(), Some(latest_context));
    assert_eq!(scheduler.pending_work_count_for_tests(), 1);

    let update = scheduler.tick(Duration::ZERO);
    assert_eq!(prepared_contexts(&update), vec![latest_context]);
}

#[test]
fn throttled_latest_live_scrub_collapses_intermediate_targets() {
    let mut scheduler = FrameScheduler::default();
    let first_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 1_000);
    let intermediate_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 2_000);
    let latest_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 3_000);
    let throttle_period =
        Duration::from_nanos(1_000_000_000 / u64::from(DEFAULT_LIVE_SCRUB_MAX_HZ));

    scheduler.submit_live_scrub_target(first_context);
    let first_start = scheduler.tick(Duration::ZERO);
    assert_eq!(prepared_contexts(&first_start), vec![first_context]);

    let throttled_intermediate = scheduler.submit_live_scrub_target(intermediate_context);
    assert_eq!(
        cancelled_contexts(&throttled_intermediate),
        vec![first_context]
    );
    assert_eq!(
        throttled_intermediate.diagnostics,
        vec![
            SchedulerDiagnostic::ActiveWorkCancelled {
                cancelled_context: first_context,
                replacement_context: Some(intermediate_context),
                reason: CancelScrubReason::SupersededByNewTarget,
            },
            SchedulerDiagnostic::LiveScrubThrottled {
                latest_context: intermediate_context,
                earliest_start: throttle_period,
            },
        ]
    );

    let throttled_latest = scheduler.submit_live_scrub_target(latest_context);
    assert_eq!(
        throttled_latest.diagnostics,
        vec![SchedulerDiagnostic::LiveScrubThrottled {
            latest_context,
            earliest_start: throttle_period,
        }]
    );
    assert_eq!(scheduler.latest_live_scrub_target(), Some(latest_context));
    assert_eq!(scheduler.pending_work_count_for_tests(), 0);
    assert!(prepared_contexts(&scheduler.tick(Duration::from_millis(1))).is_empty());

    let resumed = scheduler.tick(throttle_period);
    assert_eq!(prepared_contexts(&resumed), vec![latest_context]);
}

#[test]
fn every_drag_event_live_scrub_attempts_each_target_without_throttle_queue_growth() {
    let config = FrameServerConfig {
        live_scrub_decode_mode: LiveScrubDecodeMode::EveryDragEvent,
        ..FrameServerConfig::default()
    }
    .validate()
    .expect("valid every-drag scheduler config");
    let mut scheduler = FrameScheduler::new(config);
    let first_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 1_000);
    let second_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 2_000);
    let third_context = context_for_tests_at(ScrubRequestKind::LiveScrub, 3_000);

    scheduler.submit_live_scrub_target(first_context);
    assert_eq!(
        prepared_contexts(&scheduler.tick(Duration::ZERO)),
        vec![first_context]
    );

    let second_submit = scheduler.submit_live_scrub_target(second_context);
    assert_eq!(cancelled_contexts(&second_submit), vec![first_context]);
    assert_eq!(scheduler.pending_work_count_for_tests(), 1);
    assert_eq!(
        prepared_contexts(&scheduler.tick(Duration::ZERO)),
        vec![second_context]
    );

    let third_submit = scheduler.submit_live_scrub_target(third_context);
    assert_eq!(cancelled_contexts(&third_submit), vec![second_context]);
    assert_eq!(scheduler.pending_work_count_for_tests(), 1);
    assert_eq!(
        prepared_contexts(&scheduler.tick(Duration::ZERO)),
        vec![third_context]
    );
    assert!(
        third_submit.diagnostics.iter().all(|diagnostic| !matches!(
            diagnostic,
            SchedulerDiagnostic::LiveScrubThrottled { .. }
        ))
    );
}
