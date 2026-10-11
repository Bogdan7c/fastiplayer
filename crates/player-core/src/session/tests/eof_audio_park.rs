//! Пауза audio output-а на конце файла: после `Ended` stream не должен крутить callback-и.

use super::test_support::*;
use super::*;

/// Собирает audio-only session в EOF-drain с хвостом `tail_ms` в буфере output-а.
fn draining_session_with_audio_tail(tail_ms: f64) -> (PlayerSession, ScriptedAudioOutputHandle) {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let audio_output = install_ready_audio_runtime(&mut session, tail_ms, None);
    session.update_current_position(Duration::from_secs(30));
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();
    (session, audio_output)
}

#[test]
fn audio_output_is_paused_only_after_tail_is_played_out() {
    let (mut session, audio_output) = draining_session_with_audio_tail(200.0);

    let tick_at = Instant::now();
    let _drain_tick = session.tick(PlayerTickContext::new(tick_at));
    assert_eq!(
        session.playback_state(),
        PlaybackState::Draining,
        "хвост 200 мс ещё в буфере: Ended рано"
    );
    assert_eq!(
        audio_output.pause_count.load(Ordering::Relaxed),
        0,
        "пауза до доигрывания хвоста обрезала бы последние ~200 мс звука"
    );

    audio_output.set_buffer_level_ms(0.0);
    let _terminal_tick = session.tick(PlayerTickContext::new(tick_at + Duration::from_millis(5)));

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(
        audio_output.pause_count.load(Ordering::Relaxed),
        1,
        "на Ended output обязан уйти в backend-паузу"
    );
}

#[test]
fn parked_audio_output_asks_for_play_again_after_ended() {
    let (mut session, audio_output) = draining_session_with_audio_tail(0.0);
    let _terminal_tick = session.tick(PlayerTickContext::new(Instant::now()));
    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert_eq!(audio_output.pause_count.load(Ordering::Relaxed), 1);

    // Replay/seek после Ended возобновляют вывод через обычный play-request путь.
    assert!(session.pipeline.audio_output_needs_play_request());
    session.dispatch_command(PlayerCommand::Play).unwrap();
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
    assert!(
        session.seek_commit().is_some(),
        "Play после Ended = replay seek"
    );
}

#[test]
fn failed_pause_at_eof_does_not_block_ended() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);
    let (output, handle) = ScriptedAudioOutput::with_pause_error(0.0, "pause failed").into_parts();
    session.pipeline.install_audio_output_for_tests(output);
    session.pipeline.install_audio_clock(handle.clock());
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session.enter_eof_drain();

    let _terminal_tick = session.tick(PlayerTickContext::new(Instant::now()));

    assert_eq!(session.playback_state(), PlaybackState::Ended);
    assert!(
        session.snapshot().last_error.is_none(),
        "конец файла не fatal из-за паузы"
    );
}
