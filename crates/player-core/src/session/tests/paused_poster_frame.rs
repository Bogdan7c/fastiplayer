//! Пауза до первого кадра: на экране должен оказаться кадр-постер, а не чёрный прямоугольник.

use super::test_support::*;
use super::*;

/// Собирает video-only media на паузе без единого показанного кадра (как сразу после install).
fn paused_media_without_frame() -> (PlayerSession, SharedFakeVideoDecoderThread) {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());
    assert!(!session.pipeline.has_present_video_frame());
    (session, fake_decoder)
}

/// Пауза без кадра доводит декодер до показа первого кадра и остаётся на паузе.
#[test]
fn pause_without_any_frame_decodes_and_presents_poster_frame_and_stays_paused() {
    let (mut session, fake_decoder) = paused_media_without_frame();

    session.dispatch_command(PlayerCommand::Pause).unwrap();
    // Seek на текущую позицию (начало) принят, кадр ещё не показан.
    assert!(session.seek_commit().is_some());

    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::ZERO,
        1,
    ));
    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert_eq!(tick_result.video_frames_presented, 1);
    assert_eq!(
        session
            .pipeline
            .present_video_frame()
            .map(|frame| frame.pts),
        Some(Duration::ZERO)
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(session.snapshot().current_position, Duration::ZERO);
    assert!(session.seek_commit().is_none());
}

/// Постер берётся с текущей позиции паузы, а не с начала файла.
#[test]
fn pause_without_frame_mid_stream_seeks_to_current_position() {
    let (mut session, _fake_decoder) = paused_media_without_frame();
    session.update_current_position(Duration::from_secs(7));

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    let commit = session
        .seek_commit()
        .expect("A pause without a frame should request a poster");
    assert_eq!(commit.target_position, MediaTime::from_secs(7));
}

/// Если кадр уже на экране (обычная пауза во время воспроизведения), второй seek не запускается.
#[test]
fn pause_with_frame_on_screen_does_not_start_poster_seek() {
    let (mut session, _fake_decoder) = paused_media_without_frame();
    session
        .pipeline
        .set_present_video_frame(decoded_frame_for_tests(Duration::from_secs(1), 1));
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(
        !session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// Источник без seek-а не получает постер: seek отказал бы пользовательской ошибкой.
#[test]
fn pause_without_frame_on_non_seekable_media_neither_seeks_nor_reports_error() {
    let mut session = PlayerSession::new();
    install_fake_media_with_seekability(
        &mut session,
        vec![fake_track(1, TrackKind::Video)],
        DemuxSeekability::NotSeekable {
            reason: media_core::TimelineNotSeekableReason::SourceNotSeekable,
        },
    );
    let _ = session.take_events();

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert!(
        !session
            .take_events()
            .iter()
            .any(|event| matches!(event, PlayerEvent::SeekRequested(_)))
    );
}

/// У audio-only media нет видео, значит и постера нет: пауза остаётся обычной.
#[test]
fn pause_on_audio_only_media_does_not_start_poster_seek() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Audio)]);

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
}

/// Pause отменяет идущую перемотку и остаётся на прежней позиции; постер берётся оттуда же,
/// а не с отменённой цели.
#[test]
fn pause_cancelling_seek_without_frame_requests_poster_at_held_position_not_cancelled_target() {
    let (mut session, _fake_decoder) = paused_media_without_frame();
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(9),
        )))
        .unwrap();

    session.dispatch_command(PlayerCommand::Pause).unwrap();

    assert_eq!(
        session.seek_commit().map(|commit| commit.target_position),
        Some(MediaTime::from_duration(
            session.snapshot().current_position
        ))
    );
    assert_ne!(
        session.seek_commit().map(|commit| commit.target_position),
        Some(MediaTime::from_secs(9))
    );
}
