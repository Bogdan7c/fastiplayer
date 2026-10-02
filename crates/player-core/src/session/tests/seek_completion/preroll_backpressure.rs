//! Accurate seek: preroll, burst отправки и backpressure очередей.

use super::*;

#[test]
fn no_audio_media_seek_resumes_after_target_video_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn deselected_audio_path_seek_resumes_after_target_video_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.disable_selected_audio_path();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(6),
            42,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_016),
            43,
        ));
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_millis(6_033),
            44,
        ));
    session.note_presented_frame_for_seek(Duration::from_secs(6));

    session.finish_seek_commit_if_ready_for_tests(
        Instant::now(),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert!(session.pipeline.selected_audio_track_id().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
}

#[test]
fn active_seek_drains_target_frame_when_present_queue_is_full() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_secs(5), 5));
    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::from_secs(6),
        6,
    ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(1, 4),
    ));

    assert_eq!(tick_result.decoded_video_frames, 1);
    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(session.seek_commit().is_none());
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(6))
    );
    assert!(session.pipeline.video_present_queue_is_empty());
    assert!(!session.snapshot().timeline.stale_frame);
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| { pause.reason == crate::PipelinePauseReason::WaitingForPresentQueue })
    );
    assert!(
        fake_decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(5))
    );
}

#[test]
fn active_seek_sends_video_packet_when_present_queue_is_full() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_queued_video_frame(decoded_frame_for_current_seek_generation(
            &session,
            Duration::from_secs(6),
            60,
        ));
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_millis(6_016),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"post-seek-video"),
            true,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_present_queue: 1,
            min_video_present_queue: 1,
            target_video_present_queue: 1,
            max_video_packets_sent_per_tick: 1,
            seek_resume_video_min_ready_frames: 1,
            ..PlayerTickConfig::default()
        },
    ));

    assert_eq!(fake_decoder.sent_packets().len(), 1);
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| { pause.reason == crate::PipelinePauseReason::WaitingForPresentQueue })
    );
}

#[test]
fn active_accurate_seek_demux_reads_target_video_through_audio_preroll_backpressure() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        vec![
            fake_audio_packet(audio_track.id, target_position, Duration::from_millis(20)),
            fake_video_packet_with_keyframe(
                video_track.id,
                target_position,
                PacketKeyframe::Keyframe,
            ),
        ],
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 8,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 8,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        2,
        "accurate seek preroll должен читать target video в том же tick-е, а не останавливаться на audio packet"
    );
    assert_eq!(harness.sent_packets().len(), 1);
    assert_eq!(harness.sent_packets()[0].pts, target_position);
    assert!(
        !tick_result
            .pipeline_pauses
            .iter()
            .any(|pause| pause.reason == crate::PipelinePauseReason::DemuxBackpressure)
    );
}

#[test]
fn active_accurate_seek_demux_budget_ignores_dropped_audio_preroll() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let mut packets = Vec::new();
    for packet_index in 0..32u64 {
        packets.push(fake_audio_packet(
            audio_track.id,
            Duration::from_millis(packet_index * 20),
            Duration::from_millis(20),
        ));
    }
    packets.push(fake_video_packet_with_keyframe(
        video_track.id,
        target_position,
        PacketKeyframe::Keyframe,
    ));
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_time_budget: Duration::from_millis(50),
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        1,
        "полностью отброшенный audio preroll не должен создавать unbounded per-packet telemetry"
    );
    assert_eq!(
        tick_result.dropped_seek_audio_preroll_packets, 32,
        "полностью отброшенный audio preroll учитывается aggregate counter-ом"
    );
    assert_eq!(harness.sent_packets().len(), 1);
    assert_eq!(harness.sent_packets()[0].pts, target_position);
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("active accurate seek должен иметь diagnostics до target frame");
    assert_eq!(diagnostics.seek_mode, SeekMode::Accurate);
    assert!(diagnostics.accurate_preroll.active);
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .skipped_audio_preroll_packets,
        32
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .demux_events
            .audio_packets,
        32
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .demux_events
            .video_packets,
        1
    );
    assert!(
        diagnostics
            .accurate_preroll
            .stages
            .first_post_seek_packet_elapsed
            .is_some()
    );
    assert!(
        diagnostics
            .accurate_preroll
            .stages
            .first_target_or_after_video_packet_elapsed
            .is_some()
    );
}

#[test]
fn active_accurate_seek_interleaves_demux_and_decoder_io_during_fast_preroll() {
    let target_position = Duration::from_millis(300);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let packets = vec![
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(0),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            Duration::from_millis(0),
            PacketKeyframe::Keyframe,
        ),
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(20),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            Duration::from_millis(100),
            PacketKeyframe::NotKeyframe,
        ),
        fake_audio_packet(
            audio_track.id,
            Duration::from_millis(40),
            Duration::from_millis(20),
        ),
        fake_video_packet_with_keyframe(
            video_track.id,
            target_position,
            PacketKeyframe::NotKeyframe,
        ),
    ];
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_video_packet_burst: 1,
            seek_fast_preroll_time_budget: Duration::from_millis(50),
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        3,
        "fast-preroll loop должен записывать per-packet telemetry только для queued video packets"
    );
    assert_eq!(
        tick_result.dropped_seek_audio_preroll_packets, 3,
        "pre-target audio preroll должен оставаться aggregate diagnostics"
    );
    assert_eq!(
        harness
            .sent_packets()
            .iter()
            .map(|packet| packet.pts)
            .collect::<Vec<_>>(),
        vec![
            Duration::from_millis(0),
            Duration::from_millis(100),
            target_position
        ],
        "active accurate seek должен отправлять каждый найденный GOP packet в том же tick-е"
    );
    assert!(harness.session.pipeline.pending_video_packet_is_empty());
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());
}

#[test]
fn active_accurate_seek_without_catch_up_deadline_keeps_dropped_audio_scan_bounded() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let mut packets = Vec::new();
    for packet_index in 0..32u64 {
        packets.push(fake_audio_packet(
            audio_track.id,
            Duration::from_millis(packet_index * 20),
            Duration::from_millis(20),
        ));
    }
    packets.push(fake_video_packet_with_keyframe(
        video_track.id,
        target_position,
        PacketKeyframe::Keyframe,
    ));
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        packets,
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_time_budget: Duration::ZERO,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        tick_result.demuxed_packets.len(),
        1,
        "без catch-up deadline dropped audio preroll должен оставаться bounded обычным demux budget"
    );
    assert!(harness.sent_packets().is_empty());
    assert!(harness.session.pipeline.pending_audio_packet_is_empty());
}

#[test]
fn active_accurate_seek_sends_pre_target_video_packets_in_burst() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    for frame_index in 0..10u64 {
        let packet_pts = Duration::from_millis(frame_index * 100);
        let packet_keyframe = if frame_index == 0 {
            PacketKeyframe::Keyframe
        } else {
            PacketKeyframe::NotKeyframe
        };
        harness.session.pipeline.enqueue_pending_video_packet(
            PendingVideoPacket::new_with_decode_timestamps(
                TrackId::new(1),
                PendingVideoPacketTimestamps {
                    pts: packet_pts,
                    dts: None,
                    track_pts: None,
                    track_dts: None,
                },
                harness.session.pipeline.seek_generation(),
                Bytes::from_static(b"seek-preroll-video"),
                packet_keyframe,
            ),
        );
    }
    harness.session.pipeline.enqueue_pending_video_packet(
        PendingVideoPacket::new_with_decode_timestamps(
            TrackId::new(1),
            PendingVideoPacketTimestamps {
                pts: target_position,
                dts: None,
                track_pts: None,
                track_dts: None,
            },
            harness.session.pipeline.seek_generation(),
            Bytes::from_static(b"seek-target-video"),
            PacketKeyframe::NotKeyframe,
        ),
    );

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 16,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        harness.sent_packets().len(),
        11,
        "active accurate seek должен быстро прокачать pre-target GOP до decoder-а"
    );
    assert_eq!(tick_result.demuxed_packets.len(), 0);
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.pipeline.pending_video_packet_is_empty());

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("active accurate seek должен оставаться открыт без decoded target frame");
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .seek_video_packets_sent,
        11
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .video_preroll_packets_sent,
        10
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .target_or_after_video_packets_sent,
        1
    );
}

#[test]
fn active_accurate_seek_bypasses_audio_decode_ahead_for_target_reorder_tail() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let _audio_handle = install_ready_audio_runtime(&mut harness.session, 0.0, None);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    for (packet_pts, packet_keyframe) in [
        (Duration::from_millis(1_800), PacketKeyframe::Keyframe),
        (target_position, PacketKeyframe::NotKeyframe),
        (Duration::from_millis(2_800), PacketKeyframe::NotKeyframe),
        (Duration::from_millis(3_000), PacketKeyframe::NotKeyframe),
    ] {
        harness.session.pipeline.enqueue_pending_video_packet(
            PendingVideoPacket::new_with_decode_timestamps(
                TrackId::new(1),
                PendingVideoPacketTimestamps {
                    pts: packet_pts,
                    dts: None,
                    track_pts: None,
                    track_dts: None,
                },
                harness.session.pipeline.seek_generation(),
                Bytes::from_static(b"seek-reorder-tail-video"),
                packet_keyframe,
            ),
        );
    }

    let _tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_packets_sent_per_tick: 1,
            max_decoded_video_frames_drained_per_tick: 1,
            max_pending_video_packets: 1,
            max_pending_video_packets_during_audio_catchup: 1,
            max_video_decode_ahead: Duration::from_millis(500),
            seek_fast_preroll_video_packet_burst: 8,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(
        harness
            .sent_packets()
            .iter()
            .map(|packet| packet.pts)
            .collect::<Vec<_>>(),
        vec![
            Duration::from_millis(1_800),
            target_position,
            Duration::from_millis(2_800),
            Duration::from_millis(3_000),
        ],
        "active Accurate seek должен докачивать reorder tail после target без audio-clock pacing"
    );

    let diagnostics = harness
        .session
        .active_seek_diagnostics(Instant::now(), &seek_regression_tick_config())
        .expect("seek должен ждать decoded landing frame");
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .seek_video_packets_sent,
        4
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .video_preroll_packets_sent,
        1
    );
    assert_eq!(
        diagnostics
            .accurate_preroll
            .counters
            .target_or_after_video_packets_sent,
        3
    );
}
