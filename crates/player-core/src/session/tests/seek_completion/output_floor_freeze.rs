//! Output floor декодера, заморозка pre-seek кадра и EOF fallback.

use super::*;

#[test]
fn accurate_seek_sets_decoder_output_floor_with_target_and_generation() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_commit = harness.aligned_seek_commit();

    assert_eq!(
        harness.decoder.preroll_floor_sets(),
        vec![video_core::VideoPrerollOutputFloor {
            generation: seek_commit.generation,
            floor_pts: target_position,
            retain_latest_before_floor: true,
        }]
    );
    assert!(
        harness
            .session
            .decoder_output_floor_applies_to_seek_preroll_packet(
                actual_position,
                seek_commit.generation
            )
    );
}

#[test]
fn accurate_seek_clears_decoder_output_floor_on_commit() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::from_millis(1_500),
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_generation = harness.aligned_seek_commit().generation;
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            22,
        ));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness.decoder.preroll_floor_clears(),
        vec![video_core::VideoPrerollOutputFloorClear::MatchingGeneration(seek_generation)]
    );
}

#[test]
fn new_accurate_seek_clears_old_decoder_output_floor_generation() {
    let first_target = Duration::from_secs(2);
    let second_target = Duration::from_secs(4);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        first_target,
        Duration::from_millis(1_500),
        Vec::new(),
    )
    .with_seek_result(scripted_seek_result(
        second_target,
        Duration::from_millis(3_500),
    ));
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(first_target));
    let first_generation = harness.aligned_seek_commit().generation;
    harness.start_final_seek(MediaTime::from_duration(second_target));
    let second_generation = harness.aligned_seek_commit().generation;

    assert_ne!(first_generation, second_generation);
    assert_eq!(
        harness.decoder.preroll_floor_clears(),
        vec![video_core::VideoPrerollOutputFloorClear::MatchingGeneration(first_generation)]
    );
    assert_eq!(
        harness.decoder.preroll_floor_sets(),
        vec![
            video_core::VideoPrerollOutputFloor {
                generation: first_generation,
                floor_pts: first_target,
                retain_latest_before_floor: true,
            },
            video_core::VideoPrerollOutputFloor {
                generation: second_generation,
                floor_pts: second_target,
                retain_latest_before_floor: true,
            },
        ]
    );
}

#[test]
fn unsupported_decoder_output_floor_keeps_player_side_preroll_drop_path() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .decoder
        .push_preroll_floor_result(video_core::VideoPrerollOutputFloorResult::Unsupported);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    let seek_generation = harness.aligned_seek_commit().generation;
    assert!(
        !harness
            .session
            .decoder_output_floor_applies_to_seek_preroll_packet(actual_position, seek_generation)
    );

    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            actual_position,
            15,
        ));
    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame(),
        "unsupported decoder floor должен оставить старый player-side fallback/drop path"
    );
    assert!(harness.session.seek_commit().is_some());
}

#[test]
fn active_accurate_seek_uses_seek_specific_video_packet_burst() {
    let target_position = Duration::from_secs(4);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::ZERO,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    for frame_index in 0..20u64 {
        harness.session.pipeline.enqueue_pending_video_packet(
            PendingVideoPacket::new_with_decode_timestamps(
                TrackId::new(1),
                PendingVideoPacketTimestamps {
                    pts: Duration::from_millis(frame_index * 100),
                    dts: None,
                    track_pts: None,
                    track_dts: None,
                },
                harness.session.pipeline.seek_generation(),
                Bytes::from_static(b"seek-preroll-video"),
                if frame_index == 0 {
                    PacketKeyframe::Keyframe
                } else {
                    PacketKeyframe::NotKeyframe
                },
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
            max_pending_video_packets_during_audio_catchup: 1,
            seek_fast_preroll_video_packet_burst: 32,
            ..seek_regression_tick_config()
        },
    ));

    assert_eq!(harness.sent_packets().len(), 21);
    assert_eq!(harness.sent_packets()[20].pts, target_position);
    assert_eq!(tick_result.demuxed_packets.len(), 0);
    assert!(harness.session.pipeline.pending_video_packet_is_empty());
}

#[test]
fn final_seek_releases_stale_present_when_texture_pressure_blocks_decoder() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    fake_decoder.set_resource_snapshot(decoder_resource_snapshot_for_tests(1, 1));
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_secs(6),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"target-video"),
            true,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            min_texture_slots_available_for_decode: 0,
            max_video_packets_sent_per_tick: 1,
            ..seek_admission_tick_config(1, 4)
        },
    ));

    assert_eq!(fake_decoder.sent_packets().len(), 1);
    assert_eq!(
        fake_decoder.released_handles(),
        vec![video_core::FrameResourceHandle(1)]
    );
    assert!(session.pipeline.present_video_frame().is_none());
    assert!(session.snapshot().timeline.stale_frame);
    assert!(session.seek_commit().is_some());
    assert!(!tick_result.pipeline_pauses.iter().any(|pause| {
        matches!(
            pause.reason,
            crate::PipelinePauseReason::WaitingForFreeSurface
                | crate::PipelinePauseReason::WaitingForGpuRelease
        )
    }));
}

#[test]
fn active_seek_long_preroll_drain_keeps_present_queue_bounded() {
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

    for frame_index in 0..32u64 {
        harness
            .decoder
            .push_decoded_frame(decoded_frame_for_current_seek_generation(
                &harness.session,
                Duration::from_millis(frame_index * 50),
                100 + frame_index,
            ));
    }
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            200,
        ));

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 64),
    ));

    assert_eq!(tick_result.decoded_video_frames, 33);
    assert!(harness.session.seek_commit().is_none());
    assert!(harness.session.pipeline.video_present_queue_len() <= 2);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_position)
    );
    assert_eq!(
        tick_result.dropped_video_frames.len(),
        32,
        "весь pre-target GOP должен остаться seek-preroll, а не playback"
    );
    assert!(
        !harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );
}

#[test]
fn final_seek_retains_latest_pre_target_frame_without_landing_preview() {
    let target_position = Duration::from_secs(2);
    let first_landing_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        first_landing_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            first_landing_position,
            15,
        ));
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_900),
            19,
        ));

    let tick_result = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert_eq!(tick_result.decoded_video_frames, 2);
    // Pre-target кадр остаётся только EOF fallback candidate-ом: scheduler не показывает
    // его как preview, а ранний candidate релизится без texture leak.
    assert_eq!(tick_result.video_frames_presented, 0);
    assert_eq!(tick_result.dropped_video_frames.len(), 1);
    assert_eq!(
        harness.decoder.released_handles(),
        vec![video_core::FrameResourceHandle(15)]
    );
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    assert_eq!(harness.session.pipeline.video_present_queue_len(), 0);
    assert!(!harness.session.snapshot().timeline.stale_frame);
    assert!(harness.session.seek_commit().is_some());
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );
}

#[test]
fn accurate_seek_freezes_pre_seek_frame_then_commits_on_target_frame() {
    let target_position = Duration::from_secs(2);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        Duration::from_millis(1_500),
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);
    harness
        .session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 10));
    // max_demux_packets_per_tick: 0 держит demuxer от EOF, чтобы проверить чистый preroll path
    // без near-EOF fallback. Кадры приходят прямо из fake decoder.
    let preroll_tick_config = || PlayerTickConfig {
        max_demux_packets_per_tick: 0,
        ..seek_admission_tick_config(2, 4)
    };

    harness.start_final_seek(MediaTime::from_duration(target_position));

    // Шаг 1: первый pre-target кадр не презентуется; экран держит pre-seek кадр.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_500),
            15,
        ));
    let first_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(first_tick.video_frames_presented, 0);
    assert_eq!(first_tick.video_frames_repeated, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(1))
    );
    assert!(harness.session.seek_commit().is_some());
    assert!(harness.session.snapshot().timeline.stale_frame);
    // Позиция не сдвинута pre-target кадром: остаётся на user target до коммита.
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );

    // Шаг 2: более близкий к target кадр заменяет fallback candidate, но не present frame.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            Duration::from_millis(1_900),
            19,
        ));
    let closer_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(closer_tick.video_frames_presented, 0);
    assert_eq!(closer_tick.video_frames_repeated, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::from_secs(1))
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(15)),
        "замена fallback candidate должна релизить старый pre-target frame"
    );
    assert!(harness.session.seek_commit().is_some());

    // Шаг 3: точный target кадр заменяет pre-seek кадр, закрывает gate и коммитит target.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            target_position,
            22,
        ));
    let target_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        preroll_tick_config(),
    ));
    assert_eq!(target_tick.video_frames_presented, 1);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(target_position)
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(19)),
        "target кадр должен очистить fallback candidate без texture leak"
    );
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(10)),
        "target кадр должен релизить pre-seek present frame"
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(harness.session.snapshot().current_position, target_position);
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
}

#[test]
fn accurate_seek_keeps_audio_fully_gated_before_target_without_preview() {
    let target_position = Duration::from_secs(2);
    let actual_position = Duration::from_millis(1_500);
    let video_track = fake_track(1, TrackKind::Video);
    let audio_track = fake_track(2, TrackKind::Audio);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone(), audio_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track, audio_track], demuxer);
    let audio_handle = install_ready_audio_runtime(&mut harness.session, 0.0, None);

    // Play-intent seek: после прохождения gate-а аудио должно возобновиться, но НЕ до target.
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness.start_final_seek(MediaTime::from_duration(target_position));
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    let play_count_before = audio_handle.play_count.load(Ordering::Relaxed);

    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            actual_position,
            15,
        ));
    let preroll_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    // Pre-target video не показывается и не двигает audio/playback state.
    assert_eq!(preroll_tick.video_frames_presented, 0);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    // Аудио полностью gated: output не запускался, clock base держится на target,
    // seek ещё активен и playback не возобновлён.
    assert_eq!(
        audio_handle.play_count.load(Ordering::Relaxed),
        play_count_before
    );
    assert_eq!(harness.session.pipeline.media_clock_base(), target_position);
    assert!(harness.session.seek_commit().is_some());
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Scrubbing
    );
}

#[test]
fn keyframe_before_seek_does_not_set_accurate_output_floor() {
    let target_position = Duration::from_secs(8);
    let actual_position = Duration::from_millis(7_500);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        actual_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness
        .session
        .dispatch_command(PlayerCommand::Seek(SeekRequest {
            target: SeekTarget::Absolute(MediaTime::from_duration(target_position)),
            mode: SeekMode::KeyframeBefore,
        }))
        .unwrap();
    assert_eq!(
        harness.aligned_seek_commit().seek_mode,
        SeekMode::KeyframeBefore
    );

    assert!(
        harness.decoder.preroll_floor_sets().is_empty(),
        "KeyframeBefore не должен включать Accurate output-floor"
    );

    // Pre-target кадр KeyframeBefore не маркируется как drop-preroll и потому не оседает в
    // fallback-слоте: его путь — обычная present queue.
    let pre_target = Duration::from_millis(7_000);
    assert!(
        !harness
            .session
            .should_drop_decoded_frame_for_seek(pre_target)
    );
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            pre_target,
            15,
        ));
    let _ = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert!(
        !harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame(),
        "KeyframeBefore не должен заполнять Accurate fallback-слот"
    );
}

#[test]
fn accurate_seek_eof_commits_on_fallback_frame_after_decoder_drain() {
    let target_position = Duration::from_millis(29_500);
    let landing_position = Duration::from_secs(29);
    let video_track = fake_track(1, TrackKind::Video);
    let demuxer = scripted_seek_demuxer(
        vec![video_track.clone()],
        target_position,
        landing_position,
        Vec::new(),
    );
    let mut harness = SeekRegressionHarness::new(vec![video_track], demuxer);

    harness.start_final_seek(MediaTime::from_duration(target_position));

    // Шаг 1 (без demux/EOF): pre-target кадр сохраняется только как EOF fallback.
    harness
        .decoder
        .push_decoded_frame(decoded_frame_for_current_seek_generation(
            &harness.session,
            landing_position,
            29,
        ));
    let preroll_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            ..seek_admission_tick_config(2, 4)
        },
    ));
    assert_eq!(preroll_tick.video_frames_presented, 0);
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        None
    );
    assert!(harness.session.seek_commit().is_some());
    assert!(
        harness
            .session
            .pipeline
            .has_seek_preroll_fallback_video_frame()
    );

    // Шаг 2: demuxer доходит до EOF, decoder drain уже пуст, поэтому fallback презентуется
    // один раз и закрывает seek по near-EOF policy.
    let eof_tick = harness.session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));
    assert_eq!(
        eof_tick.video_frames_presented, 1,
        "EOF fallback должен презентоваться только после decoder drain"
    );
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness
            .session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(landing_position)
    );
    assert_eq!(
        harness.session.snapshot().current_position,
        landing_position
    );
    assert_eq!(
        harness.session.pipeline.media_clock_base(),
        landing_position
    );
    assert!(!harness.session.snapshot().timeline.seeking);
    assert!(!harness.session.snapshot().timeline.stale_frame);
}
