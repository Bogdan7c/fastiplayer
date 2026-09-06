//! Регрессии длинного live drag через demux → decoder → presented frame → resume.

use super::test_support::*;
use super::*;

fn continuous_scrub_harness() -> SeekRegressionHarness {
    let mut harness = continuous_video_harness(vec![fake_track(1, TrackKind::Video)]);
    begin_drag(&mut harness);
    harness
}

fn continuous_video_harness(tracks: Vec<TrackInfo>) -> SeekRegressionHarness {
    let video = &tracks[0];
    let packets = (0..500)
        .map(|index| {
            fake_video_packet_with_keyframe(
                video.id,
                Duration::from_millis(8_000 + index * 40),
                if index == 0 {
                    PacketKeyframe::Keyframe
                } else {
                    PacketKeyframe::NotKeyframe
                },
            )
        })
        .collect();
    let demuxer = scripted_seek_demuxer(
        tracks.clone(),
        Duration::from_secs(8),
        Duration::from_secs(8),
        packets,
    );
    let harness = SeekRegressionHarness::new(tracks, demuxer);
    for index in 0..500 {
        harness
            .decoder
            .decode_next_packet_as_frame(Duration::from_millis(8_000 + index * 40), 1_000 + index);
    }
    harness
}

fn begin_drag(harness: &mut SeekRegressionHarness) {
    harness
        .session
        .dispatch_command(PlayerCommand::Play)
        .unwrap();
    harness
        .session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    preview(harness, 8_000);
}

fn preview(harness: &mut SeekRegressionHarness, target_ms: u64) {
    harness
        .session
        .dispatch_command(PlayerCommand::preview_scrub(SeekRequest::absolute(
            MediaTime::from_duration(Duration::from_millis(target_ms)),
        )))
        .unwrap();
}

fn tick_at(harness: &mut SeekRegressionHarness, now: Instant) -> PlayerTickResult {
    harness.session.tick(PlayerTickContext::with_config(
        now,
        PlayerTickConfig {
            seek_fast_preroll_video_packet_burst: 16,
            ..seek_regression_fast_preroll_tick_config()
        },
    ))
}

fn presented_ms(harness: &SeekRegressionHarness) -> u128 {
    harness
        .session
        .pipeline
        .present_video_frame()
        .unwrap()
        .pts
        .as_millis()
}

#[test]
fn live_drag_holds_landing_with_bounded_decode_and_resumes_after_release() {
    let mut harness = continuous_scrub_harness();
    let now = Instant::now();
    for step in 0..100 {
        let tick = tick_at(&mut harness, now + Duration::from_millis(step * 100));
        assert_eq!(presented_ms(&harness), 8_000);
        assert!(
            tick.dropped_video_frames
                .iter()
                .all(|frame| { frame.reason != PlayerVideoDropReason::QueueOverflow })
        );
    }
    assert!(harness.session.snapshot().timeline.scrubbing);
    assert!(!harness.session.is_eof_draining());
    assert!(harness.sent_packets().len() < 20);
    assert!(harness.decoder.released_handles().is_empty());

    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .unwrap();
    for _ in 0..4 {
        harness.tick_once_fast_preroll();
    }
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Playing
    );
    tick_at(&mut harness, Instant::now() + Duration::from_millis(200));
    assert!(presented_ms(&harness) > 8_000);
    assert!(
        harness
            .decoder
            .released_handles()
            .contains(&video_core::FrameResourceHandle(1_000))
    );
}

#[test]
fn live_drag_tiny_forward_updates_keep_nearest_presented_frame_and_generation() {
    let mut harness = continuous_scrub_harness();
    harness.tick_once_fast_preroll();
    let generation = harness.aligned_seek_commit().generation;
    for target_ms in 8_001..8_201 {
        preview(&mut harness, target_ms);
        harness.tick_once_fast_preroll();
        let visible = presented_ms(&harness);
        assert!(visible >= u128::from(target_ms));
        assert!(
            visible - u128::from(target_ms) < 40,
            "target={target_ms}, visible={visible}"
        );
        assert_eq!(harness.aligned_seek_commit().generation, generation);
        assert!(!harness.session.snapshot().timeline.stale_frame);
    }
    assert_eq!(harness.seek_requests().len(), 1);
    assert!(!harness.session.is_eof_draining());
    preview(&mut harness, 8_100);
    assert_ne!(harness.aligned_seek_commit().generation, generation);
    assert_eq!(harness.seek_requests().len(), 2);
}

#[test]
fn live_drag_release_waits_for_audio_then_resumes_output_and_video() {
    let mut harness = continuous_video_harness(vec![
        fake_track(1, TrackKind::Video),
        fake_track(2, TrackKind::Audio),
    ]);
    let audio = install_ready_audio_runtime(&mut harness.session, 80.0, None);
    begin_drag(&mut harness);
    audio.set_buffer_level_ms(0.0);
    let play_count = audio.play_count.load(Ordering::Relaxed);
    for _ in 0..10 {
        harness.tick_once_fast_preroll();
    }
    assert_eq!(presented_ms(&harness), 8_000);
    assert_eq!(audio.play_count.load(Ordering::Relaxed), play_count);
    assert!(audio.pause_count.load(Ordering::Relaxed) > 0);

    harness
        .session
        .dispatch_command(PlayerCommand::end_scrub(
            ScrubCommitPolicy::CommitLatestTarget,
        ))
        .unwrap();
    harness.tick_once_fast_preroll();
    assert!(harness.session.seek_commit().is_some());
    assert_eq!(audio.play_count.load(Ordering::Relaxed), play_count);

    // Output boundary подтверждает накопленный PCM; только теперь release
    // имеет право запускать звук и продолжать выдачу video frames.
    audio.set_buffer_level_ms(80.0);
    for _ in 0..4 {
        harness.tick_once_fast_preroll();
    }
    assert!(harness.session.seek_commit().is_none());
    assert_eq!(
        harness.session.snapshot().playback_state,
        PlaybackState::Playing
    );
    assert_eq!(audio.play_count.load(Ordering::Relaxed), play_count + 1);
    audio.clock.record_played(19_200);
    harness.tick_once_fast_preroll();
    assert!(presented_ms(&harness) > 8_000);
}
