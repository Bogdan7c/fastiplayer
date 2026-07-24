use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use media_core::{DemuxSeekRequest, DemuxSeekResult, MediaTime, TrackKind};

use super::test_support::{fake_track, install_fake_media};
use super::*;
use crate::{
    PlaybackState, PreparedDemuxSeekEnqueueError, PreparedDemuxSeekMode, PreparedDemuxSeekOutcome,
    PreparedDemuxSeekPort, PreparedDemuxSeekReceipt, PreparedDemuxSeekRequestId,
};

/// Deterministic nonblocking fake exact prepared-media seek port-а.
#[derive(Default)]
struct FakePreparedDemuxSeekPort {
    /// Accepted commands для request/fence assertions.
    commands: Mutex<Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)>>,
    /// Test-owned terminal receipts.
    receipts: Mutex<VecDeque<PreparedDemuxSeekReceipt>>,
}

impl FakePreparedDemuxSeekPort {
    /// Публикует terminal outcome exact accepted request-а.
    fn complete(&self, request_id: PreparedDemuxSeekRequestId, outcome: PreparedDemuxSeekOutcome) {
        self.receipts
            .lock()
            .expect("fake receipt lock")
            .push_back(PreparedDemuxSeekReceipt {
                request_id,
                outcome,
            });
    }

    /// Снимает immutable accepted request snapshot.
    fn commands(&self) -> Vec<(PreparedDemuxSeekRequestId, DemuxSeekRequest)> {
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

/// Устанавливает seekable video fake и exact worker-receipted mode.
fn receipted_session() -> (PlayerSession, Arc<FakePreparedDemuxSeekPort>) {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Audio)]);
    let port = Arc::new(FakePreparedDemuxSeekPort::default());
    let erased: Arc<dyn PreparedDemuxSeekPort> = port.clone();
    session
        .prepared_demux_seek
        .install(PreparedDemuxSeekMode::WorkerReceipted { port: erased });
    (session, port)
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
