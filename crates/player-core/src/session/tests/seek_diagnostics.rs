use super::test_support::*;
use super::*;

#[test]
fn active_seek_diagnostics_identifies_demux_blocker_before_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let diagnostics = session
        .active_seek_diagnostics(
            Instant::now() + Duration::from_millis(300),
            &PlayerTickConfig::default(),
        )
        .expect("active seek diagnostics should exist while seek commit is open");

    assert_eq!(diagnostics.kind, "seek");
    assert_eq!(diagnostics.target, Duration::from_secs(5));
    assert_eq!(
        diagnostics.blocker,
        crate::SeekProgressBlocker::WaitingForDemux
    );
    assert!(!diagnostics.target_frame_presented);
    assert_eq!(diagnostics.queues.present_queue_depth, 0);
}

#[test]
fn seek_progress_blocker_reports_post_flush_keyframe_drops() {
    let mut session = PlayerSession::new();

    session.pipeline.require_video_decoder_keyframe();
    session.record_video_decoder_bootstrap_started();
    session.record_video_packet_dropped_until_keyframe();

    let queues = PipelineQueueDepthSnapshot::default();
    let seek_bootstrap = session
        .diagnostics
        .snapshot_with_queues(queues)
        .seek_bootstrap;
    let blocker = session.video_target_frame_blocker(queues, seek_bootstrap);

    assert_eq!(blocker, SeekProgressBlocker::WaitingForPostFlushKeyframe);
}

#[test]
fn active_seek_diagnostics_reports_audio_preroll_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    let gate_snapshot = SeekProgressGateSnapshot {
        target_frame_presented: true,
        video_gate_ready: true,
        audio_gate_status: SeekAudioGateStatus::WaitingForPreroll,
        ready_video_frames: 1,
        required_video_frames: 1,
    };

    let blocker = session.seek_progress_blocker(
        &PlayerTickConfig::default(),
        PipelineQueueDepthSnapshot::default(),
        gate_snapshot,
        SeekBootstrapDiagnosticsSnapshot::default(),
    );

    assert_eq!(blocker, SeekProgressBlocker::WaitingForAudioPreroll);
}
