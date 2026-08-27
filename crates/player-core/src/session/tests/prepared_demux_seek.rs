use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use media_core::{DemuxSeekRequest, DemuxSeekResult, MediaTime, PacketKeyframe, TrackKind};

use super::test_support::{
    SeekRegressionHarness, fake_track, fake_video_packet_with_keyframe, install_fake_media,
    scripted_seek_demuxer,
};
use super::*;
use crate::{
    PlaybackState, PlayerError, PlayerErrorKind, PlayerEvent, PreparedDemuxSeekEnqueueError,
    PreparedDemuxSeekMode, PreparedDemuxSeekOutcome, PreparedDemuxSeekPort,
    PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};

/// Deterministic nonblocking fake exact prepared-media seek port-а.
#[derive(Default)]
pub(super) struct FakePreparedDemuxSeekPort {
    /// Accepted commands для request/fence assertions.
    commands: Mutex<Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)>>,
    /// Test-owned terminal receipts.
    receipts: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
}

/// Prepared port, который детерминированно моделирует уже остановленный worker.
struct WorkerStoppedPreparedDemuxSeekPort;

impl PreparedDemuxSeekPort for WorkerStoppedPreparedDemuxSeekPort {
    fn enqueue_seek(
        &self,
        _request_id: PreparedDemuxSeekRequestId,
        _request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        Err(PreparedDemuxSeekEnqueueError::WorkerStopped)
    }

    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        None
    }
}

impl FakePreparedDemuxSeekPort {
    /// Публикует terminal outcome exact accepted request-а.
    pub(super) fn complete(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        outcome: PreparedDemuxSeekOutcome,
    ) {
        self.receipts
            .lock()
            .expect("fake receipt lock")
            .push_back(PreparedDemuxSeekReceipt {
                request_id,
                outcome,
            });
    }

    /// Снимает immutable accepted request snapshot.
    pub(super) fn commands(&self) -> Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)> {
        self.commands.lock().expect("fake command lock").clone()
    }
}

impl PreparedDemuxSeekPort for FakePreparedDemuxSeekPort {
    /// Fake принимает monotonic requests без blocking work.
    fn enqueue_seek(
        &self,
        request_id: PreparedDemuxSeekRequestId,
        request: DemuxSeekRequest,
    ) -> Result<(), PreparedDemuxSeekEnqueueError> {
        self.commands
            .lock()
            .expect("fake command lock")
            .push((request_id, request));
        Ok(())
    }

    /// Fake возвращает каждый terminal receipt at-most-once.
    fn poll_seek_receipt(&self) -> Option<PreparedDemuxSeekReceipt> {
        self.receipts.lock().expect("fake receipt lock").pop_front()
    }
}

/// Устанавливает seekable audio fake и exact worker-receipted mode.
fn receipted_session() -> (PlayerSession, Arc<FakePreparedDemuxSeekPort>) {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Audio)]);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    (session, port)
}

/// Устанавливает seekable media с prepared port-ом, который отклоняет enqueue.
fn worker_stopped_receipted_session() -> PlayerSession {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Audio)]);
    let port: Arc<dyn PreparedDemuxSeekPort> = Arc::new(WorkerStoppedPreparedDemuxSeekPort);
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    session
}

#[test]
fn worker_stopped_enqueue_preserves_causal_failed_state_and_error() {
    let mut session = worker_stopped_receipted_session();
    let causal_error = PlayerError::new(PlayerErrorKind::DemuxError, "causal demux failure");
    session.mark_fatal_error(causal_error.clone());
    let _prior_events = session.take_events();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(11),
        )))
        .expect("secondary public seek remains handled by player state machine");

    assert_eq!(session.playback_state(), PlaybackState::Failed);
    assert_eq!(session.snapshot().last_error.as_ref(), Some(&causal_error));
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.snapshot().timeline.target_position.is_none());
    assert!(
        session
            .take_events()
            .into_iter()
            .all(|event| !matches!(event, PlayerEvent::RecoverableError(_)))
    );
}

#[test]
fn worker_stopped_enqueue_remains_recoverable_from_non_fatal_states() {
    for initial_state in [PlaybackState::Paused, PlaybackState::Playing] {
        let mut session = worker_stopped_receipted_session();
        session.set_playback_state(initial_state);
        let _prior_events = session.take_events();

        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(13),
            )))
            .expect("ordinary prepared enqueue failure remains recoverable");

        assert_eq!(session.playback_state(), PlaybackState::Paused);
        let error = session
            .snapshot()
            .last_error
            .clone()
            .expect("recoverable enqueue failure must remain visible");
        assert_eq!(error.kind, PlayerErrorKind::SeekUnavailable);
        assert!(error.message.contains("demux seek worker has stopped"));
        assert!(session.take_events().into_iter().any(|event| matches!(
            event,
            PlayerEvent::RecoverableError(recoverable) if recoverable == error
        )));
    }
}

/// Устанавливает video fake, чтобы проверить production one-shot route и отсутствие sync seek-а.
pub(super) fn receipted_video_session() -> (
    PlayerSession,
    Arc<FakePreparedDemuxSeekPort>,
    Arc<Mutex<Vec<Duration>>>,
) {
    let mut session = PlayerSession::new();
    let synchronous_seek_log =
        install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    (session, port, synchronous_seek_log)
}

#[test]
fn legacy_media_without_port_keeps_synchronous_seek() {
    let mut session = PlayerSession::new();
    let seek_log = install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(4),
        )))
        .expect("legacy seek command");

    assert_eq!(
        seek_log.lock().expect("seek log lock").as_slice(),
        &[Duration::from_secs(4)]
    );
    assert!(session.seek_commit().is_some());
}

#[test]
fn video_one_shot_seek_uses_worker_receipt_without_synchronous_demux_scan() {
    let (mut session, port, synchronous_seek_log) = receipted_video_session();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .expect("video receipted seek command");

    let commands = port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("video one-shot seek должен создать ровно один worker request");
    };
    assert_eq!(request.timestamp, Duration::from_secs(8));
    assert!(
        synchronous_seek_log
            .lock()
            .expect("synchronous seek log lock")
            .is_empty(),
        "video one-shot seek не должен сканировать active demuxer синхронно"
    );
    assert!(session.seek_commit().is_none());
    assert_eq!(session.playback_state(), PlaybackState::Seeking);

    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(8),
            actual_position: MediaTime::from_secs(5),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();

    let commit = session
        .seek_commit()
        .expect("authoritative worker receipt должен запустить video landing commit");
    assert_eq!(commit.target_position, MediaTime::from_secs(8));
    assert_eq!(commit.actual_position, MediaTime::from_secs(5));
    assert!(
        synchronous_seek_log
            .lock()
            .expect("synchronous seek log lock")
            .is_empty(),
        "receipt acceptance не должен повторять demux seek на player-owner"
    );
}

#[test]
fn worker_receipted_video_seek_reaches_target_frame_presentation() {
    let target_position = Duration::from_secs(8);
    let actual_position = Duration::from_secs(5);
    let landing_position = Duration::from_millis(8_040);
    let video_track = fake_track(1, TrackKind::Video);
    let packets = vec![
        fake_video_packet_with_keyframe(video_track.id, actual_position, PacketKeyframe::Keyframe),
        fake_video_packet_with_keyframe(
            video_track.id,
            landing_position,
            PacketKeyframe::NotKeyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(actual_position, 901);
    harness
        .decoder
        .decode_next_packet_as_frame(landing_position, 902);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let commands = port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("public video seek должен создать один worker request");
    };
    assert_eq!(request.timestamp, target_position);
    assert!(
        harness.seek_requests().is_empty(),
        "active demuxer не должен выполнять второй синхронный seek"
    );

    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(actual_position),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();

    let mut presented_frames = 0;
    for _ in 0..6 {
        presented_frames += harness.tick_once_fast_preroll().video_frames_presented;
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    assert!(
        presented_frames > 0,
        "receipt должен привести не только к commit state, но и к presentation target-frame"
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position)
    );
    assert!(harness.seek_requests().is_empty());
    assert!(harness.session.seek_commit().is_none());
}

/// Release timeline scrub-а не должен коммитить preview-якорь вместо worker receipt-а.
#[test]
fn worker_receipted_scrub_release_suppresses_preroll_until_target_frame_presentation() {
    let target_position = Duration::from_secs(8);
    let preview_anchor = Duration::from_secs(1);
    let receipted_anchor = Duration::from_secs(5);
    let landing_position = Duration::from_millis(8_040);
    let video_track = fake_track(1, TrackKind::Video);
    let packets = vec![
        fake_video_packet_with_keyframe(video_track.id, receipted_anchor, PacketKeyframe::Keyframe),
        fake_video_packet_with_keyframe(
            video_track.id,
            landing_position,
            PacketKeyframe::NotKeyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        preview_anchor,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    harness
        .session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted {
            port: erased,
            landing_policy: crate::PreparedDemuxSeekLandingPolicy::DecodeForwardToTarget,
        });
    harness
        .decoder
        .decode_next_packet_as_frame(receipted_anchor, 911);
    harness
        .decoder
        .decode_next_packet_as_frame(landing_position, 912);

    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .expect("begin worker-receipted scrub");
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(target_position),
        )))
        .expect("preview worker-receipted scrub");
    assert!(harness.session.active_seek_presents_preroll_progressively());
    assert_eq!(
        harness.seek_requests().as_slice(),
        &[DemuxSeekRequest::decode_point_before(target_position)],
        "preview сохраняет прежний неблокирующий progressive route"
    );

    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .expect("release worker-receipted scrub");

    let commands = port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("EndScrub должен создать ровно один authoritative worker request");
    };
    assert_eq!(request.timestamp, target_position);
    assert!(harness.session.seek_commit().is_none());
    assert!(!harness.session.active_seek_presents_preroll_progressively());
    assert_eq!(
        harness.seek_requests().len(),
        1,
        "release не должен повторять seek через preview controller"
    );

    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_duration(target_position),
            actual_position: MediaTime::from_duration(receipted_anchor),
            actual_track_timestamp: None,
        }),
    );
    harness.session.service_prepared_demux_seek_receipts();

    assert!(
        harness
            .session
            .should_drop_decoded_frame_for_seek(receipted_anchor),
        "после release кадры от anchor до target должны идти только в decoder preroll"
    );
    let mut presented_frame_positions = Vec::new();
    for _ in 0..6 {
        let tick_result = harness.tick_once_fast_preroll();
        if tick_result.video_frames_presented > 0 {
            let presented_position = harness
                .session
                .pipeline
                .present_video_frame()
                .expect("presentation counter требует current frame")
                .pts;
            presented_frame_positions.push(presented_position);
        }
        if harness.session.seek_commit().is_none() {
            break;
        }
    }

    assert!(!presented_frame_positions.is_empty());
    assert!(
        presented_frame_positions
            .iter()
            .all(|position| *position >= target_position),
        "после release ни один pre-target кадр не должен стать видимым: {presented_frame_positions:?}"
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position),
        "первый видимый итог release-а обязан быть target-or-after frame"
    );
    assert!(harness.session.seek_commit().is_none());
}

#[test]
fn authoritative_receipt_starts_existing_commit_without_premature_position_publication() {
    let (mut session, port) = receipted_session();

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(8),
        )))
        .expect("receipted seek command");

    let commands = port.commands();
    let [(request_id, request)] = commands.as_slice() else {
        panic!("exact one seek command expected");
    };
    assert_eq!(request.timestamp, Duration::from_secs(8));
    assert!(session.seek_commit().is_none());
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::ZERO
    );

    port.complete(
        *request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(8),
            actual_position: MediaTime::from_secs(3),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();

    let commit = session.seek_commit().expect("receipt starts normal commit");
    assert_eq!(commit.target_position, MediaTime::from_secs(8));
    assert_eq!(commit.actual_position, MediaTime::from_secs(3));
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::ZERO
    );
}

#[test]
fn failed_or_cancelled_receipt_never_commits_position() {
    for outcome in [
        PreparedDemuxSeekOutcome::Failed,
        PreparedDemuxSeekOutcome::Cancelled,
        PreparedDemuxSeekOutcome::Superseded,
        PreparedDemuxSeekOutcome::Stale,
    ] {
        let (mut session, port) = receipted_session();
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(6),
            )))
            .expect("receipted seek command");
        let request_id = port.commands()[0].0;

        port.complete(request_id, outcome);
        session.service_prepared_demux_seek_receipts();

        assert!(session.seek_commit().is_none());
        assert_eq!(
            session.snapshot().timeline.current_position,
            MediaTime::ZERO
        );
        assert_eq!(session.playback_state(), PlaybackState::Paused);
    }
}

#[test]
fn mismatched_requested_target_never_enters_seek_commit() {
    let (mut session, port) = receipted_session();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .expect("receipted seek command");
    let request_id = port.commands()[0].0;

    port.complete(
        request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(7),
            actual_position: MediaTime::from_secs(3),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();

    assert!(session.seek_commit().is_none());
    assert_eq!(
        session.snapshot().timeline.current_position,
        MediaTime::ZERO
    );
    assert_eq!(session.playback_state(), PlaybackState::Paused);
}

#[test]
fn rapid_seek_accepts_only_latest_exact_receipt() {
    let (mut session, port) = receipted_session();
    for target in [2_u64, 9_u64] {
        session
            .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
                MediaTime::from_secs(target),
            )))
            .expect("rapid seek command");
    }
    let commands = port.commands();
    assert_eq!(commands.len(), 2);

    port.complete(
        commands[0].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(2),
            actual_position: MediaTime::from_secs(1),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    assert!(session.seek_commit().is_none());

    port.complete(
        commands[1].0,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(9),
            actual_position: MediaTime::from_secs(7),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    let commit = session.seek_commit().expect("latest receipt commits");
    assert_eq!(commit.target_position, MediaTime::from_secs(9));
    assert_eq!(commit.actual_position, MediaTime::from_secs(7));
}

#[test]
fn generation_change_and_media_reset_drop_late_receipts() {
    let (mut session, port) = receipted_session();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .expect("receipted seek command");
    let request_id = port.commands()[0].0;
    session.pipeline.begin_seek_generation();
    port.complete(
        request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(5),
            actual_position: MediaTime::from_secs(4),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    assert!(session.seek_commit().is_none());

    session.reset_media_state();
    port.complete(
        request_id,
        PreparedDemuxSeekOutcome::Succeeded(DemuxSeekResult {
            requested_position: MediaTime::from_secs(5),
            actual_position: MediaTime::from_secs(4),
            actual_track_timestamp: None,
        }),
    );
    session.service_prepared_demux_seek_receipts();
    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().media_instance_id, None);
}
