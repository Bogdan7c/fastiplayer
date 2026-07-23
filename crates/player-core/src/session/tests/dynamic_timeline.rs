use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};

use crossbeam_channel::bounded;
use media_core::{
    DynamicMediaTimelineEpoch, DynamicMediaTimelineInitial, DynamicMediaTimelinePortGeneration,
    DynamicMediaTimelineState, MediaTime, TimelineMode, TimelineNotSeekableReason, TimelineRange,
    dynamic_media_timeline,
};

use super::test_support::FakeDemuxer;
use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, PlayerErrorKind, PlayerSession,
    PreparedMedia, PreparedMediaTimelineModeError, TimelineSeekKind, TimelineSeekRequestId,
};

fn generation(value: u64) -> DynamicMediaTimelinePortGeneration {
    DynamicMediaTimelinePortGeneration::new(
        NonZeroU64::new(value).expect("test generation must be non-zero"),
    )
}

fn live_media(
    generation_value: u64,
    state: DynamicMediaTimelineState,
) -> (PreparedMedia, media_core::DynamicMediaTimelinePublisher) {
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(generation_value),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state,
    });
    let demuxer = FakeDemuxer::new(Vec::new(), None, Arc::new(Mutex::new(Vec::new())));
    let prepared_media = PreparedMedia::from_external_label("fake-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("duration-less fake accepts live timeline");
    (prepared_media, publisher)
}

fn dvr_state(start: u64, end: u64) -> DynamicMediaTimelineState {
    let range = TimelineRange::new(MediaTime::from_secs(start), MediaTime::from_secs(end))
        .expect("ordered test range");
    DynamicMediaTimelineState::with_dvr(MediaTime::from_secs(end), range)
        .expect("valid test DVR state")
}

#[test]
fn no_dvr_live_is_durationless_and_non_seekable() {
    let (prepared_media, _publisher) = live_media(
        1,
        DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(40)),
    );
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    let snapshot = session.snapshot();
    assert_eq!(snapshot.timeline.mode, TimelineMode::Live);
    assert_eq!(snapshot.duration, None);
    assert_eq!(snapshot.timeline.duration, None);
    assert_eq!(snapshot.timeline.live_edge, Some(MediaTime::from_secs(40)));
    assert!(!snapshot.timeline.seekable);
    assert_eq!(
        snapshot.timeline.not_seekable_reason,
        Some(TimelineNotSeekableReason::LiveWindowUnavailable)
    );
}

#[test]
fn sliding_non_zero_dvr_updates_public_range_without_moving_playback_position() {
    let (prepared_media, publisher) = live_media(2, dvr_state(20, 60));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    assert_eq!(
        session.snapshot().current_position,
        MediaTime::from_secs(60).as_duration()
    );

    publisher
        .publish(DynamicMediaTimelineEpoch::new(2), dvr_state(30, 70))
        .expect("sliding publication");
    assert!(session.refresh_dynamic_timeline());

    let snapshot = session.snapshot();
    assert_eq!(
        snapshot.timeline.seekable_range,
        TimelineRange::new(MediaTime::from_secs(30), MediaTime::from_secs(70))
    );
    assert_eq!(snapshot.timeline.live_edge, Some(MediaTime::from_secs(70)));
    assert_eq!(
        snapshot.current_position,
        MediaTime::from_secs(60).as_duration()
    );
}

#[test]
fn wait_source_does_not_apply_unpublished_revision_before_arm_recheck() {
    let (prepared_media, publisher) = live_media(8, dvr_state(20, 60));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);

    publisher
        .publish(DynamicMediaTimelineEpoch::new(2), dvr_state(30, 70))
        .expect("sliding publication");
    let wait_source = session
        .dynamic_timeline_wait_source()
        .expect("active live wait source");

    assert_eq!(
        session.snapshot().timeline.live_edge,
        Some(MediaTime::from_secs(60)),
        "observe/arm descriptor must not silently mutate public session output"
    );
    assert!(session.dynamic_timeline_changed_after_arm(
        wait_source.port_generation,
        wait_source.observed_revision,
    ));
    assert_eq!(
        session.snapshot().timeline.live_edge,
        Some(MediaTime::from_secs(70))
    );
}

#[test]
fn static_window_and_live_mode_are_typed_mutually_exclusive() {
    let (port, _publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(3),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state: DynamicMediaTimelineState::without_dvr(MediaTime::from_secs(5)),
    });
    let demuxer = FakeDemuxer::new(Vec::new(), None, Arc::new(Mutex::new(Vec::new())));
    let live = PreparedMedia::from_external_label("fake-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("live mode");
    let static_window =
        crate::MediaPlaybackWindow::new(MediaTime::ZERO, Some(MediaTime::from_secs(2)))
            .expect("static window");

    assert!(matches!(
        live.with_playback_window(static_window),
        Err(PreparedMediaTimelineModeError::PlaybackWindowConflictsWithLiveTimeline)
    ));
}

#[test]
fn exact_target_expiry_is_typed_and_never_false_applied() {
    let (prepared_media, publisher) = live_media(4, dvr_state(20, 60));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let media_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("installed live media");
    let request_id = TimelineSeekRequestId::new(NonZeroU64::new(9).expect("request id"));
    let (outcome_tx, outcome_rx) = bounded(1);
    session.begin_exact_timeline_seek(
        ExactTimelineSeekRequest {
            request_id,
            media_instance_id,
            target: MediaTime::from_secs(25),
            kind: TimelineSeekKind::SetPosition,
        },
        outcome_tx,
    );

    publisher
        .publish(DynamicMediaTimelineEpoch::new(2), dvr_state(30, 70))
        .expect("window expiry publication");
    assert!(session.refresh_dynamic_timeline());

    assert!(matches!(
        outcome_rx.recv().expect("typed expiry outcome"),
        ExactTimelineSeekOutcome::Expired {
            request_id: expired_request,
            requested_position,
            ..
        } if expired_request == request_id && requested_position == MediaTime::from_secs(25)
    ));
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .expect("expiry is recoverable")
            .kind,
        PlayerErrorKind::SeekTargetExpired
    );
}

#[test]
fn old_port_after_replace_cannot_mutate_new_live_media() {
    let (first_media, first_publisher) = live_media(5, dvr_state(10, 50));
    let (second_media, _second_publisher) = live_media(6, dvr_state(100, 140));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(first_media, false);
    session.load_prepared_media_with_autoplay(second_media, false);

    first_publisher
        .publish(DynamicMediaTimelineEpoch::new(2), dvr_state(20, 70))
        .expect_err("old port consumer is disconnected after replacement");
    assert!(!session.refresh_dynamic_timeline());
    assert_eq!(
        session.snapshot().timeline.seekable_range,
        TimelineRange::new(MediaTime::from_secs(100), MediaTime::from_secs(140))
    );
}

#[test]
fn disconnected_publisher_disables_activity_but_keeps_last_valid_snapshot() {
    let (prepared_media, publisher) = live_media(7, dvr_state(40, 80));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let wait_source = session
        .dynamic_timeline_wait_source()
        .expect("active live wait source");
    let retained_range = session.snapshot().timeline.seekable_range;

    drop(publisher);
    assert!(wait_source.activity_receiver.recv().is_err());
    session.disconnect_dynamic_timeline_activity(wait_source.port_generation);

    assert!(session.dynamic_timeline_wait_source().is_none());
    assert_eq!(session.snapshot().timeline.seekable_range, retained_range);
}
