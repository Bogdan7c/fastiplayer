use super::test_support::*;
use super::*;

fn enqueue_diagnostics_probe_packets(session: &mut PlayerSession) {
    let generation = session.pipeline.seek_generation();

    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            TrackId::new(1),
            Duration::from_millis(10),
            None,
            None,
            generation,
            Bytes::from_static(b"audio-diagnostics"),
        ));
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(2),
            Duration::from_millis(20),
            generation,
            Bytes::from_static(b"video-diagnostics"),
            true,
        ));
}

#[test]
fn diagnostics_reset_when_media_state_resets() {
    let mut session = PlayerSession::default();

    session.record_video_drop(
        Some(Duration::from_millis(40)),
        crate::VideoDropReason::Late,
    );
    assert_eq!(session.diagnostics_snapshot().drops.total, 1);

    session.reset_media_state();

    let diagnostics_after_reset = session.diagnostics_snapshot();
    assert_eq!(diagnostics_after_reset.drops.total, 0);
    assert_eq!(diagnostics_after_reset.drops.late, 0);
    assert_eq!(diagnostics_after_reset.drops.last, None);
}

#[test]
fn diagnostics_queue_snapshot_does_not_mutate_pipeline() {
    let mut session = PlayerSession::default();
    enqueue_diagnostics_probe_packets(&mut session);

    let diagnostics = session.diagnostics_snapshot();

    assert_eq!(diagnostics.queues.pending_audio_packets, 1);
    assert_eq!(diagnostics.queues.pending_video_packets, 1);
    assert_eq!(session.pipeline.pending_audio_packet_len(), 1);
    assert_eq!(session.pipeline.pending_video_packet_len(), 1);
}

#[test]
fn diagnostics_queue_snapshot_reports_video_recovery_staging_without_mutation() {
    let mut session = PlayerSession::default();
    let selected_video_track = TrackId::new(2);
    session.pipeline.select_video_track(
        selected_video_track,
        VideoDecodeRequirement::new(VideoCodec::Av1),
    );
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            selected_video_track,
            Duration::ZERO,
            session.pipeline.seek_generation(),
            Bytes::from_static(b"recovery-proof-keyframe"),
            PacketKeyframe::Keyframe,
        ));
    let _proof_keyframe = session
        .pipeline
        .pop_pending_video_packet_front()
        .expect("proof keyframe должен считаться уже переданным decoder-у");
    session.pipeline.mark_video_decoder_bootstrapped();
    assert_eq!(
        session.pipeline.begin_video_backlog_recovery_scan(
            crate::pipeline::VideoBacklogRecoveryScanLimits::for_tests()
        ),
        crate::pipeline::VideoBacklogRecoveryScanStart::Started
    );
    assert_eq!(
        session
            .pipeline
            .route_pending_video_packet_for_backlog_recovery(PendingVideoPacket::new(
                selected_video_track,
                Duration::from_millis(10),
                session.pipeline.seek_generation(),
                Bytes::from_static(b"staged-diagnostics-packet"),
                PacketKeyframe::NotKeyframe,
            )),
        crate::pipeline::VideoBacklogRecoveryRouteOutcome::StagedWhileScanning
    );

    let diagnostics = session.diagnostics_snapshot();

    assert_eq!(diagnostics.queues.staged_video_backlog_recovery_packets, 1);
    assert_eq!(
        diagnostics.queues.staged_video_backlog_recovery_bytes,
        b"staged-diagnostics-packet".len()
    );
    assert!(session.pipeline.video_backlog_recovery_scan_allows_demux());
    assert_eq!(
        session.pipeline.video_backlog_recovery_staged_packet_len(),
        1
    );
}

#[test]
fn diagnostics_records_latency_drop_and_wakeup_with_queue_attribution() {
    let mut session = PlayerSession::default();
    enqueue_diagnostics_probe_packets(&mut session);

    let worker_wakeup = crate::WorkerWakeupDiagnosticsSnapshot {
        reason: Some(crate::WorkerWakeupReason::PipelineWorkReady),
        planned_delay: Some(Duration::from_millis(5)),
        tick_late_by: Duration::from_millis(2),
        frame_timing: None,
    };

    session.record_pipeline_latency(
        crate::PipelineLatencyStage::WorkerScheduler,
        Duration::from_millis(7),
        Some(Duration::from_millis(20)),
        None,
    );
    session.record_video_drop(
        Some(Duration::from_millis(20)),
        crate::VideoDropReason::QueueOverflow,
    );
    session.record_worker_wakeup(worker_wakeup);

    let diagnostics = session.diagnostics_snapshot();
    let latency_sample = diagnostics
        .worst_latencies
        .worker_scheduler
        .worst
        .expect("worker scheduler latency sample должен сохраниться");
    let drop_attribution = diagnostics
        .drops
        .last
        .expect("video drop attribution должен сохраниться");

    assert_eq!(latency_sample.queues.pending_audio_packets, 1);
    assert_eq!(latency_sample.queues.pending_video_packets, 1);
    assert_eq!(drop_attribution.queues.pending_audio_packets, 1);
    assert_eq!(drop_attribution.queues.pending_video_packets, 1);
    assert_eq!(diagnostics.worker_wakeup, worker_wakeup);
    assert_eq!(diagnostics.queues.pending_audio_packets, 1);
    assert_eq!(diagnostics.queues.pending_video_packets, 1);
}

#[test]
fn diagnostics_log_summary_treats_decoder_control_pressure_as_activity() {
    let mut session = PlayerSession::default();
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    let control_pressure = DecoderControlChannelPressureSnapshot {
        control_channel_len: 32,
        control_channel_capacity: 32,
        control_channel_full_count: 1,
        release_control_send_fail_count: 0,
        flush_control_send_fail_count: 0,
    };
    fake_decoder.set_control_pressure(control_pressure);
    session.pipeline.set_video_decoder_thread(fake_decoder);

    let summary = session.diagnostics_log_summary();

    assert!(summary.has_activity());
    assert_eq!(
        summary.queues.decoder_control_channel,
        Some(control_pressure)
    );
}
