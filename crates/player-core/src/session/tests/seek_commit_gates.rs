use super::test_support::*;
use super::*;

#[test]
fn seek_audio_gate_treats_no_selected_audio_as_ready() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    let gate_status = classify_seek_audio_gate(
        seek_commit,
        AudioSeekRuntimeState::NoSelectedAudio,
        seek_commit.generation,
        None,
        50.0,
    );

    assert_eq!(gate_status, SeekAudioGateStatus::Ready);
}

#[test]
fn seek_audio_gate_preserves_clear_decoder_output_and_preroll_blockers() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation - 1,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForClear
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::WaitingForDecoder,
            seek_commit.generation,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::WaitingForOutput,
            seek_commit.generation,
            Some(100.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForOutput
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            None,
            50.0,
        ),
        SeekAudioGateStatus::WaitingForPreroll
    );
}

#[test]
fn final_play_seek_audio_gate_requires_minimal_preroll() {
    let seek_commit = audio_gate_seek_commit(PlaybackResumeIntent::Play);

    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            Some(49.0),
            50.0,
        ),
        SeekAudioGateStatus::WaitingForPreroll
    );
    assert_eq!(
        classify_seek_audio_gate(
            seek_commit,
            AudioSeekRuntimeState::Ready,
            seek_commit.generation,
            Some(50.0),
            50.0,
        ),
        SeekAudioGateStatus::Ready
    );
}

#[test]
fn paused_seek_audio_gate_skips_runtime_preroll_after_clear() {
    let paused_commit = audio_gate_seek_commit(PlaybackResumeIntent::Pause);

    assert_eq!(
        classify_seek_audio_gate(
            paused_commit,
            AudioSeekRuntimeState::WaitingForOutput,
            paused_commit.generation,
            None,
            50.0,
        ),
        SeekAudioGateStatus::Ready
    );
}

#[test]
fn video_audio_seek_waits_for_audio_runtime_after_clear_ack() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("seek commit должен остаться открытым до audio runtime readiness");

    assert_eq!(
        session.pipeline.audio_buffer_clear_generation(),
        seek_commit.generation
    );
    assert_eq!(
        session.pipeline.audio_seek_runtime_state(),
        AudioSeekRuntimeState::WaitingForDecoder
    );
    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));
}

#[test]
fn audio_only_seek_does_not_commit_when_output_is_absent() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(2, TrackKind::Audio)]);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(5),
        )))
        .unwrap();

    let seek_commit = session
        .seek_commit()
        .expect("audio-only seek должен ждать audio runtime readiness");

    assert_eq!(
        session.seek_audio_gate_status(seek_commit, 50.0),
        SeekAudioGateStatus::WaitingForDecoder
    );
    assert!(!session.seek_audio_gate_ready(seek_commit, 50.0));

    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        1,
    );

    assert!(session.seek_commit().is_some());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Seeking);
}

#[test]
fn playing_video_seek_with_audio_soft_fallback_commits_after_target_frame() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

    session.dispatch_command(PlayerCommand::Play).unwrap();
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    present_frame_for_current_seek_generation(&mut session, Duration::from_secs(6), 42);

    let seek_commit = session
        .seek_commit()
        .expect("seek должен ждать audio до soft fallback deadline");
    session.finish_seek_commit_if_ready_for_tests(
        seek_commit.started_at + Duration::from_millis(250),
        Duration::from_secs(10),
        50.0,
        Duration::from_millis(250),
        3,
    );

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert!(!session.snapshot().timeline.seeking);
    assert!(session.seek_commit().is_none());
    assert!(matches!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .map(|error| &error.kind),
        Some(PlayerErrorKind::RuntimeError)
    ));

    let events = session.take_events();
    let target_frame_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == Duration::from_secs(6)
                        && presentation.frame_pts == Duration::from_secs(6)
            )
        },
        "target frame event должен быть опубликован до soft fallback commit",
    );
    let commit_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekCommitted(commit)
                    if commit.target_position == Duration::from_secs(6)
                        && commit.actual_position == Duration::from_secs(6)
                        && commit.resume_intent == PlaybackResumeIntent::Play
            )
        },
        "seek commit event должен быть опубликован после soft fallback",
    );
    assert!(target_frame_event_index < commit_event_index);
    assert!(events.iter().any(|event| matches!(
        event,
        PlayerEvent::RecoverableError(error)
            if error.kind == PlayerErrorKind::RuntimeError
                && error.message.contains("blocker=audio_decoder")
    )));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::AudioResumedAfterSeek(_)))
    );
}

#[test]
fn paused_video_audio_seek_does_not_wait_for_audio_runtime() {
    let mut session = PlayerSession::new();
    install_fake_media(
        &mut session,
        vec![
            fake_track(1, TrackKind::Video),
            fake_track(2, TrackKind::Audio),
        ],
    );

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
        3,
    );

    assert!(session.seek_commit().is_none());
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(session.snapshot().current_position, Duration::from_secs(6));
    assert!(!session.snapshot().timeline.seeking);

    let events = session.take_events();
    let target_frame_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekTargetFramePresented(presentation)
                    if presentation.target_position == Duration::from_secs(6)
                        && presentation.frame_pts == Duration::from_secs(6)
            )
        },
        "paused seek должен опубликовать target frame event",
    );
    let commit_event_index = event_index(
        &events,
        |event| {
            matches!(
                event,
                PlayerEvent::SeekCommitted(commit)
                    if commit.target_position == Duration::from_secs(6)
                        && commit.resume_intent == PlaybackResumeIntent::Pause
            )
        },
        "paused seek должен опубликовать commit event",
    );
    assert!(target_frame_event_index < commit_event_index);
}
