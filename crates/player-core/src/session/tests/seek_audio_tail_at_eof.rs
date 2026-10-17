//! Audio gate seek-а к самому концу media (backlog #21).
//!
//! Если demux уже дошёл до EOF и весь прочитанный звук передан decoder-у, новых audio
//! данных не будет: gate не должен вечно ждать preroll, а seek обязан закоммититься и
//! дать EOF drain-у штатно довести media до конца. Пока звук может прийти — gate ждёт.

use super::test_support::*;
use super::*;

/// Минимальный post-seek audio preroll, который просит tick config.
const RESUME_AUDIO_MIN_BUFFER_MS: f64 = 50.0;

/// Уровень output buffer-а ниже preroll-а: остаток хвоста короче требуемого запаса.
const SHORT_TAIL_BUFFER_MS: f64 = 10.0;

/// Audio-only media 30 с, Play и seek почти в конец; audio runtime уже создан.
fn seek_to_end_with_short_audio_tail(session: &mut PlayerSession) -> SeekCommitState {
    install_fake_media(session, vec![fake_track(2, TrackKind::Audio)]);
    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_millis(29_990),
        )))
        .unwrap();
    install_ready_audio_runtime(session, SHORT_TAIL_BUFFER_MS, None);
    session
        .seek_commit()
        .expect("seek to the end waits for its audio gate")
}

/// Ставит в pending queue ещё не декодированный audio packet текущей generation.
fn enqueue_undecoded_audio_tail(session: &mut PlayerSession) {
    session
        .pipeline
        .enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
            TrackId::new(2),
            Duration::from_millis(29_990),
            None,
            Some(Duration::from_millis(10)),
            session.pipeline.seek_generation(),
            Bytes::from_static(b"audio-tail"),
        ));
}

/// Пробует закрыть seek тем же путём, что tick.
fn try_finish_seek(session: &mut PlayerSession, seek_commit: SeekCommitState) {
    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_secs(10),
        RESUME_AUDIO_MIN_BUFFER_MS,
        Duration::from_millis(250),
        1,
    );
}

#[test]
fn seek_to_end_commits_when_eof_leaves_no_more_audio_for_preroll() {
    let mut session = PlayerSession::new();
    let seek_commit = seek_to_end_with_short_audio_tail(&mut session);

    session.enter_eof_drain();

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, RESUME_AUDIO_MIN_BUFFER_MS),
        SeekAudioGateStatus::Ready
    );
    try_finish_seek(&mut session, seek_commit);
    assert!(
        session.seek_commit().is_none(),
        "the seek must not hang in WaitingForAudioPreroll at EOF"
    );
    assert_ne!(session.playback_state(), PlaybackState::Seeking);
}

#[test]
fn seek_to_end_still_waits_while_read_audio_is_not_decoded() {
    let mut session = PlayerSession::new();
    let seek_commit = seek_to_end_with_short_audio_tail(&mut session);
    enqueue_undecoded_audio_tail(&mut session);

    session.enter_eof_drain();

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, RESUME_AUDIO_MIN_BUFFER_MS),
        SeekAudioGateStatus::WaitingForPreroll
    );
    try_finish_seek(&mut session, seek_commit);
    assert!(session.seek_commit().is_some());
}

#[test]
fn seek_before_eof_keeps_waiting_for_audio_preroll() {
    let mut session = PlayerSession::new();
    let seek_commit = seek_to_end_with_short_audio_tail(&mut session);

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, RESUME_AUDIO_MIN_BUFFER_MS),
        SeekAudioGateStatus::WaitingForPreroll
    );
    try_finish_seek(&mut session, seek_commit);
    assert!(session.seek_commit().is_some());
    assert_eq!(session.playback_state(), PlaybackState::Seeking);
}

#[test]
fn eof_does_not_bypass_missing_audio_buffer_clear_ack() {
    let mut session = PlayerSession::new();
    let seek_commit = seek_to_end_with_short_audio_tail(&mut session);
    session.enter_eof_drain();
    // Commit более новой generation, чей clear ack output ещё не подтвердил.
    let newer_generation_commit = SeekCommitState {
        generation: seek_commit.generation + 1,
        ..seek_commit
    };

    assert_eq!(
        session.seek_audio_gate_status(newer_generation_commit, RESUME_AUDIO_MIN_BUFFER_MS),
        SeekAudioGateStatus::WaitingForClear
    );
}
