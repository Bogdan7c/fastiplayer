use std::collections::VecDeque;
use std::num::NonZeroU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crossbeam_channel::bounded;
use media_core::{
    DemuxSeekMode, DemuxSeekRequest, DemuxSeekResult, DynamicMediaTimelineEpoch,
    DynamicMediaTimelineInitial, DynamicMediaTimelinePortGeneration, DynamicMediaTimelineState,
    MediaTime, TimelineMode, TimelineNotSeekableReason, TimelineRange, TrackKind,
    dynamic_media_timeline,
};

use super::test_support::{FakeDemuxer, fake_track};
use crate::seek_state::{PlaybackResumeIntent, SeekTargetRetention};
use crate::{
    ExactTimelineSeekOutcome, ExactTimelineSeekRequest, PlaybackState, PlayerCommand,
    PlayerErrorKind, PlayerSession, PreparedDemuxSeekEnqueueError, PreparedDemuxSeekMode,
    PreparedDemuxSeekOutcome, PreparedDemuxSeekPort, PreparedDemuxSeekReceipt,
    PreparedDemuxSeekRequestId, PreparedMedia, PreparedMediaTimelineModeError, TimelineSeekKind,
    TimelineSeekRequestId,
};

/// Nonblocking fake доказывает, что live recovery использует prepared worker boundary.
#[derive(Default)]
struct RecoveryPreparedSeekPort {
    /// Exact команды, принятые boundary.
    commands: Mutex<Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)>>,
    /// Terminal receipts, которыми управляет test owner.
    receipts: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
}

impl RecoveryPreparedSeekPort {
    /// Возвращает immutable snapshot принятых команд.
    fn commands(&self) -> Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)> {
        self.commands.lock().expect("recovery command lock").clone()
    }

    /// Публикует authoritative terminal receipt.
    fn complete(&self, request_id: PreparedDemuxSeekRequestId, outcome: PreparedDemuxSeekOutcome) {
        self.receipts
            .lock()
            .expect("recovery receipt lock")
            .push_back(PreparedDemuxSeekReceipt {
                request_id,
                outcome,
            });
    }
}

impl PreparedDemuxSeekPort for RecoveryPreparedSeekPort {
    /// Enqueue остаётся nonblocking и сохраняет exact request identity.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.commands
            .lock()
            .expect("recovery command lock")
            .push((request_id, request));
        Ok(())
    }

    /// Player забирает каждый terminal receipt ровно один раз.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.receipts
            .lock()
            .expect("recovery receipt lock")
            .pop_front()
    }
}

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

/// Play після довгої pause оформляє window expiry як seek, а не timestamp gap.
#[test]
fn play_seeks_to_fresh_live_edge_when_paused_position_expired() {
    let (port, publisher) = dynamic_media_timeline(DynamicMediaTimelineInitial {
        port_generation: generation(9),
        source_epoch: DynamicMediaTimelineEpoch::new(1),
        state: dvr_state(20, 60),
    });
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(
        vec![fake_track(1, TrackKind::Video)],
        None,
        Arc::clone(&seek_log),
    );
    let prepared_media = PreparedMedia::from_external_label("fake-live", Box::new(demuxer))
        .with_dynamic_timeline(port)
        .expect("duration-less fake accepts live timeline");
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let prepared_seek_port = Arc::new(RecoveryPreparedSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });

    let fresh_availability =
        TimelineRange::new(MediaTime::from_secs(70), MediaTime::from_secs(110))
            .expect("fresh availability");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::with_available_dvr(
                MediaTime::from_secs(110),
                fresh_availability,
            )
            .expect("unproven fresh availability"),
        )
        .expect("expired-window publication");
    assert!(session.refresh_dynamic_timeline());
    assert_eq!(
        session.snapshot().current_position,
        MediaTime::from_secs(60).as_duration(),
        "paused refresh alone must not create repeated sliding seeks"
    );

    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play recovery command");

    assert!(
        seek_log.lock().expect("seek log mutex").is_empty(),
        "video live recovery must not call synchronous demux seek"
    );
    let commands = prepared_seek_port.commands();
    assert_eq!(commands.len(), 1);
    assert_eq!(commands[0].1.timestamp, Duration::from_secs(110));
    assert_eq!(commands[0].1.mode, DemuxSeekMode::DecodePointBefore);
    assert_eq!(session.playback_state(), PlaybackState::Seeking);

    let receipt_wait_availability =
        TimelineRange::new(MediaTime::from_secs(70), MediaTime::from_secs(112))
            .expect("receipt-wait availability");
    let receipt_wait_packet_proof =
        TimelineRange::new(MediaTime::from_secs(111), MediaTime::from_secs(112))
            .expect("receipt-wait packet proof");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(3),
            DynamicMediaTimelineState::with_available_and_seekable_dvr(
                MediaTime::from_secs(112),
                receipt_wait_availability,
                receipt_wait_packet_proof,
            )
            .expect("receipt-wait timeline"),
        )
        .expect("receipt-wait publication");
    assert!(session.refresh_dynamic_timeline());
    assert!(
        session.prepared_demux_seek.receipt_pending(),
        "packet proof must not expire a live-availability target before worker receipt"
    );
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(session.snapshot().last_error.is_none());

    prepared_seek_port.complete(
        commands[0].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(110),
            actual_position: MediaTime::from_secs(108),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    let seek_commit = session
        .seek_runtime
        .active_commit()
        .expect("recovery seek commit");
    assert_eq!(seek_commit.target_position, MediaTime::from_secs(110));
    assert_eq!(seek_commit.resume_intent, PlaybackResumeIntent::Play);
    assert_eq!(
        seek_commit.target_retention,
        SeekTargetRetention::LiveAvailability
    );

    let commit_availability =
        TimelineRange::new(MediaTime::from_secs(70), MediaTime::from_secs(113))
            .expect("commit availability");
    let commit_packet_proof =
        TimelineRange::new(MediaTime::from_secs(112), MediaTime::from_secs(113))
            .expect("commit packet proof");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(4),
            DynamicMediaTimelineState::with_available_and_seekable_dvr(
                MediaTime::from_secs(113),
                commit_availability,
                commit_packet_proof,
            )
            .expect("active-commit timeline"),
        )
        .expect("active-commit publication");
    assert!(session.refresh_dynamic_timeline());
    assert!(
        session.seek_runtime.active_commit().is_some(),
        "packet proof must not expire a live-availability target after worker receipt"
    );
    assert!(session.snapshot().last_error.is_none());

    let expired_availability =
        TimelineRange::new(MediaTime::from_secs(111), MediaTime::from_secs(120))
            .expect("expired recovery availability");
    let expired_packet_proof =
        TimelineRange::new(MediaTime::from_secs(119), MediaTime::from_secs(120))
            .expect("expired recovery packet proof");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(5),
            DynamicMediaTimelineState::with_available_and_seekable_dvr(
                MediaTime::from_secs(120),
                expired_availability,
                expired_packet_proof,
            )
            .expect("expired recovery timeline"),
        )
        .expect("expired recovery publication");
    assert!(session.refresh_dynamic_timeline());
    assert!(session.seek_runtime.active_commit().is_none());
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(&PlayerErrorKind::SeekTargetExpired),
        "live recovery must still expire when authoritative availability drops its target"
    );
}

/// Availability expiry до worker receipt-а терминально закрывает начатый recovery seek.
#[test]
fn live_recovery_expiry_before_worker_receipt_returns_to_paused() {
    let (prepared_media, publisher) = live_media(10, dvr_state(20, 60));
    let mut session = PlayerSession::new();
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let prepared_seek_port = Arc::new(RecoveryPreparedSeekPort::default());
    let erased_port: Arc<dyn PreparedDemuxSeekPort> = prepared_seek_port.clone();
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased_port,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });

    let recovery_availability =
        TimelineRange::new(MediaTime::from_secs(70), MediaTime::from_secs(110))
            .expect("recovery availability");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(2),
            DynamicMediaTimelineState::with_available_dvr(
                MediaTime::from_secs(110),
                recovery_availability,
            )
            .expect("recovery timeline"),
        )
        .expect("recovery publication");
    assert!(session.refresh_dynamic_timeline());
    session
        .dispatch_command(PlayerCommand::Play)
        .expect("Play recovery command");
    let command = prepared_seek_port
        .commands()
        .into_iter()
        .next()
        .expect("pending recovery command");
    assert!(session.prepared_demux_seek.receipt_pending());

    let expired_availability =
        TimelineRange::new(MediaTime::from_secs(111), MediaTime::from_secs(120))
            .expect("expired availability");
    publisher
        .publish(
            DynamicMediaTimelineEpoch::new(3),
            DynamicMediaTimelineState::with_available_dvr(
                MediaTime::from_secs(120),
                expired_availability,
            )
            .expect("expired timeline"),
        )
        .expect("expired publication");
    assert!(session.refresh_dynamic_timeline());

    assert!(!session.prepared_demux_seek.receipt_pending());
    assert!(session.seek_runtime.active_commit().is_none());
    assert_eq!(session.playback_state(), PlaybackState::Paused);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(session.snapshot().timeline.target_position, None);
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(&PlayerErrorKind::SeekTargetExpired)
    );

    prepared_seek_port.complete(
        command.0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(110),
            actual_position: MediaTime::from_secs(108),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    assert!(session.seek_runtime.active_commit().is_none());
    assert_eq!(session.playback_state(), PlaybackState::Paused);
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
