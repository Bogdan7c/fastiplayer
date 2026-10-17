//! EOF drain не ждёт кадр за концом media после доигранного звука (backlog #21).
//!
//! Живой D6: reopen ровно на конец 20 с HLS-ролика оставлял в очереди кадр 20.04 с
//! (B-кадры сдвигают PTS), audio clock после доигранного звука стоял на 20.0 с, и session
//! навсегда оставалась в `Draining` вместо `Ended` (очередь плейлиста не шла дальше).

use super::test_support::*;
use super::*;

/// Duration fake media.
const MEDIA_DURATION: Duration = Duration::from_secs(30);

/// Media с video+audio, дошедшая до EOF: звук доигран, decoder дожат.
fn drained_session_at_media_end(known_duration: Option<Duration>) -> PlayerSession {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );
    session.set_snapshot_duration(known_duration);
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    decoder.set_eof_drain_state(video_core::VideoDecoderEndOfStreamDrainState::Drained {
        generation: session.pipeline.seek_generation(),
    });
    // Пустой output buffer: звук хвоста уже полностью доигран.
    install_ready_audio_runtime(&mut session, 0.0, None);
    session.update_current_position(MEDIA_DURATION);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    session
}

/// Кладёт в present queue кадр текущей generation.
fn queue_frame(session: &mut PlayerSession, pts: Duration) {
    let frame = decoded_frame_for_current_seek_generation(session, pts, 2_104);
    session.pipeline.enqueue_queued_video_frame(frame);
}

/// Несколько обычных tick-ов EOF drain-а.
fn tick_drain(session: &mut PlayerSession) {
    let started_at = Instant::now();
    for tick_index in 0..3 {
        let _tick_result = session.tick(PlayerTickContext::new(
            started_at + Duration::from_millis(tick_index),
        ));
    }
}

#[test]
fn drained_audio_and_frame_past_media_end_finish_playback() {
    let mut session = drained_session_at_media_end(Some(MEDIA_DURATION));
    queue_frame(&mut session, MEDIA_DURATION + Duration::from_millis(40));

    tick_drain(&mut session);

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(session.snapshot().current_position, MEDIA_DURATION);
}

#[test]
fn frame_past_end_keeps_drain_open_when_media_end_is_unknown() {
    let mut session = drained_session_at_media_end(None);
    queue_frame(&mut session, MEDIA_DURATION + Duration::from_millis(40));

    tick_drain(&mut session);

    assert_eq!(
        session.playback_state(),
        PlaybackState::Draining,
        "without a known end the queued frame may still become due"
    );
    assert_eq!(session.pipeline.video_present_queue_len(), 1);
}
