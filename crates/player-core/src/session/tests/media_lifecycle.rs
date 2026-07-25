use super::test_support::*;
use super::*;

#[test]
fn active_seek_generation_is_rebased_after_track_list_update() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let seek_before_reset = session
        .seek_commit()
        .expect("seek должен быть активен после accepted demux seek");
    let generation_before_reset = seek_before_reset.generation;

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_track(3, TrackKind::Video)],
        Some(Duration::from_secs(45)),
    ));

    let seek_after_reset = session
        .seek_commit()
        .expect("TracksChanged не должен закрывать active seek");
    assert_eq!(
        seek_after_reset.generation,
        session.pipeline.seek_generation()
    );
    assert_ne!(seek_after_reset.generation, generation_before_reset);
    assert_eq!(
        seek_after_reset.target_position,
        seek_before_reset.target_position
    );
    assert_eq!(
        seek_after_reset.actual_position,
        seek_before_reset.actual_position
    );
    assert_eq!(
        seek_after_reset.resume_intent,
        seek_before_reset.resume_intent
    );
    assert!(session.snapshot().timeline.scrubbing);
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(6))
    );
}

#[test]
fn active_seek_survives_tracks_changed_before_first_video_packet() {
    let mut session = PlayerSession::new();
    let initial_tracks = vec![fake_track(1, TrackKind::Video)];
    let reset_tracks = vec![fake_track(3, TrackKind::Video)];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        initial_tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::clone(&seek_log),
    );
    demuxer.push_event(DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
        reset_tracks.clone(),
        Some(Duration::from_secs(30)),
    )));
    demuxer.push_event(DemuxReadEvent::Packet(fake_video_packet(
        TrackId::new(3),
        Duration::from_secs(6),
    )));
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, initial_tracks.clone());
    session
        .select_default_video_track(
            &initial_tracks,
            "fake media lifecycle media содержит video track",
        )
        .expect("fake media lifecycle video track должен получить fresh decode requirement");
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.apply_demux_seekability(DemuxSeekability::Seekable);
    session.set_playback_state(PlaybackState::Paused);
    let fake_decoder = SharedFakeVideoDecoderThread::new();
    session
        .pipeline
        .set_video_decoder_thread(fake_decoder.clone());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .unwrap();
    let generation_after_demux_seek = session
        .seek_commit()
        .expect("accepted seek должен открыть commit")
        .generation;

    let demux_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            max_video_packets_sent_per_tick: 1,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    let seek_after_reset = session
        .seek_commit()
        .expect("post-reset packet ещё ждёт decoded target frame");
    assert_eq!(demux_tick.demuxed_packets.len(), 1);
    assert_eq!(demux_tick.demuxed_packets[0].track_id, TrackId::new(3));
    assert_eq!(fake_decoder.sent_packets().len(), 1);
    assert_eq!(
        seek_after_reset.generation,
        session.pipeline.seek_generation()
    );
    assert_ne!(seek_after_reset.generation, generation_after_demux_seek);

    fake_decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
        &session,
        Duration::from_secs(6),
        60,
    ));
    let present_tick = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        seek_admission_tick_config(2, 4),
    ));

    assert_eq!(present_tick.video_frames_presented, 1);
    assert!(session.seek_commit().is_none());
    assert!(!session.snapshot().timeline.seeking);
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        vec![Duration::from_secs(6)]
    );
}

#[test]
fn stale_packets_from_old_generation_are_dropped_after_track_list_update() {
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
    let stale_generation = session
        .seek_commit()
        .expect("seek должен быть активен")
        .generation;

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_track(1, TrackKind::Video)],
        Some(Duration::from_secs(30)),
    ));
    assert_ne!(stale_generation, session.pipeline.seek_generation());
    session
        .pipeline
        .enqueue_pending_video_packet(PendingVideoPacket::new(
            TrackId::new(1),
            Duration::from_secs(6),
            stale_generation,
            Bytes::from_static(b"stale-video-packet"),
            true,
        ));

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 0,
            max_video_packets_sent_per_tick: 1,
            ..seek_admission_tick_config(2, 4)
        },
    ));

    assert!(fake_decoder.sent_packets().is_empty());
    assert!(session.pipeline.pending_video_packet_is_empty());
    assert_eq!(
        tick_result.dropped_video_frames,
        vec![PlayerVideoFrameDrop {
            pts: Duration::from_secs(6),
            reason: crate::PlayerVideoDropReason::StaleGeneration,
        }]
    );
}

#[test]
fn preview_scrub_does_not_create_seek_commit_for_track_list_reset() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![fake_track(1, TrackKind::Video)]);
    session
        .dispatch_command(PlayerCommand::begin_scrub())
        .unwrap();
    let request = SeekRequest::absolute(MediaTime::from_secs(7));
    session
        .dispatch_command(PlayerCommand::UpdateScrub(request))
        .unwrap();
    session
        .dispatch_command(PlayerCommand::preview_scrub(request))
        .unwrap();

    assert!(session.seek_commit().is_some());
    assert!(session.snapshot().timeline.scrubbing);
    assert_eq!(
        session.snapshot().timeline.target_position,
        Some(MediaTime::from_secs(7))
    );

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_track(3, TrackKind::Video)],
        Some(Duration::from_secs(45)),
    ));

    assert!(session.seek_commit().is_none());
    assert!(!session.snapshot().timeline.scrubbing);
    assert_eq!(session.simple_scrub_latest_request_for_tests(), None);
}

#[test]
fn prepared_media_install_publishes_open_snapshot_and_events() {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_track(1, TrackKind::Video)];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), Arc::clone(&seek_log))
        .with_seekability(DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        });
    let media_path = std::path::PathBuf::from("/tmp/sample.webm");
    let prepared_media = PreparedMedia::from_local_file(media_path.clone(), Box::new(demuxer));

    session.load_prepared_media_with_autoplay(prepared_media, false);

    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    assert_eq!(
        session.snapshot().source_label.as_deref(),
        Some("/tmp/sample.webm")
    );
    assert_eq!(session.snapshot().media_title.as_deref(), Some("sample"));
    assert_eq!(session.snapshot().duration, Some(Duration::from_secs(30)));
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1))
    );
    assert!(!session.snapshot().timeline.seekable);
    assert_eq!(
        session.snapshot().timeline.not_seekable_reason,
        Some(TimelineNotSeekableReason::SourceNotSeekable)
    );

    let events = session.take_events();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::MediaOpenRequested(request)
                if request.source == MediaSource::LocalFile(media_path.clone())
                    && !request.autoplay
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::MediaOpened(summary)
                if summary.source_label == "/tmp/sample.webm"
                    && summary.title.as_deref() == Some("sample")
                    && summary.duration == Some(Duration::from_secs(30))
        )
    }));
}

#[test]
fn pending_old_event_is_not_relabelled_after_new_media_install() {
    let mut session = PlayerSession::new();
    let old_demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(10)),
        Arc::new(Mutex::new(Vec::new())),
    );
    session.load_prepared_media_with_autoplay(
        PreparedMedia::from_external_label("old-instance".to_owned(), Box::new(old_demuxer)),
        false,
    );
    let old_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("old media must have exact instance identity");
    let _ = session.take_correlated_events();

    session.push_player_event(PlayerEvent::PositionChanged(Duration::from_secs(1)));

    let new_demuxer = FakeDemuxer::new(
        Vec::new(),
        Some(Duration::from_secs(20)),
        Arc::new(Mutex::new(Vec::new())),
    );
    session.load_prepared_media_with_autoplay(
        PreparedMedia::from_external_label("new-instance".to_owned(), Box::new(new_demuxer)),
        false,
    );
    let new_instance_id = session
        .snapshot()
        .media_instance_id
        .expect("new media must have exact instance identity");
    let correlated_events = session.take_correlated_events();

    assert_ne!(old_instance_id, new_instance_id);
    assert!(correlated_events.iter().any(|correlated_event| {
        correlated_event.media_instance_id == Some(old_instance_id)
            && correlated_event.event == PlayerEvent::PositionChanged(Duration::from_secs(1))
    }));
    assert!(correlated_events.iter().any(|correlated_event| {
        correlated_event.media_instance_id == Some(new_instance_id)
            && matches!(
                &correlated_event.event,
                PlayerEvent::MediaOpened(summary) if summary.source_label == "new-instance"
            )
    }));
}

#[test]
fn audio_only_prepared_media_opens_without_missing_video_fatal() {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_audio_track_with_codec(2, "A_FLAC")];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), Arc::clone(&seek_log));
    let media_path = std::path::PathBuf::from("/tmp/song.flac");
    let prepared_media = PreparedMedia::from_local_file(media_path.clone(), Box::new(demuxer));

    session.load_prepared_media_with_autoplay(prepared_media, false);

    let snapshot = session.snapshot_with_frame_counters(FrameCounters::default());
    assert_eq!(snapshot.playback_state, PlaybackState::Paused);
    assert_eq!(snapshot.source_label.as_deref(), Some("/tmp/song.flac"));
    assert_eq!(snapshot.media_title.as_deref(), Some("song.flac"));
    assert_eq!(snapshot.duration, Some(Duration::from_secs(30)));
    assert_eq!(snapshot.selected_tracks.video_track, None);
    assert_eq!(snapshot.current_video_frame, None);
    assert!(snapshot.last_error.is_none());
    assert!(session.pipeline.has_demuxer());

    let events = session.take_events();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::MediaOpenRequested(request)
                if request.source == MediaSource::LocalFile(media_path.clone())
                    && !request.autoplay
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::MediaOpened(summary)
                if summary.source_label == "/tmp/song.flac"
                    && summary.title.as_deref() == Some("song")
                    && summary.duration == Some(Duration::from_secs(30))
        )
    }));
}

#[test]
fn failed_prepared_media_open_publishes_error_without_resetting_old_playback() {
    let mut session = PlayerSession::new();
    let old_tracks = vec![fake_track(1, TrackKind::Video)];
    let old_seek_log = Arc::new(Mutex::new(Vec::new()));
    let old_demuxer = FakeDemuxer::new(
        old_tracks,
        Some(Duration::from_secs(30)),
        Arc::clone(&old_seek_log),
    );
    let old_media_path = std::path::PathBuf::from("/tmp/current.webm");
    let old_prepared_media =
        PreparedMedia::from_local_file(old_media_path.clone(), Box::new(old_demuxer));
    session.load_prepared_media_with_autoplay(old_prepared_media, false);
    let _ = session.take_events();
    session.set_playback_state(PlaybackState::Playing);

    let failed_request = MediaOpenRequest::new(
        MediaSource::LocalFile(std::path::PathBuf::from("/tmp/missing.webm")),
        true,
    );
    let open_error = PlayerError::new(PlayerErrorKind::DemuxError, "adapter failed");

    session.fail_media_open_with_error(failed_request.clone(), open_error.clone());

    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);
    assert_eq!(
        session.snapshot().source_label.as_deref(),
        Some("/tmp/current.webm")
    );
    assert_eq!(session.snapshot().media_title.as_deref(), Some("current"));
    assert_eq!(
        session.snapshot().selected_tracks.video_track,
        Some(TrackId::new(1))
    );
    assert!(session.pipeline.has_demuxer());
    assert_eq!(
        session
            .snapshot()
            .last_error
            .as_ref()
            .expect("open failure должен попасть в snapshot")
            .kind,
        PlayerErrorKind::DemuxError
    );

    let events = session.take_events();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::MediaOpenRequested(request) if request == &failed_request
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::RecoverableError(error)
                if error.kind == open_error.kind && error.message == open_error.message
        )
    }));
    assert!(
        !events
            .iter()
            .any(|event| matches!(event, PlayerEvent::FatalError(_)))
    );
}

#[test]
fn not_seekable_prepared_media_blocks_seek_transaction() {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_track(1, TrackKind::Video)];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let demuxer = FakeDemuxer::new(tracks, Some(Duration::from_secs(30)), Arc::clone(&seek_log))
        .with_seekability(DemuxSeekability::NotSeekable {
            reason: TimelineNotSeekableReason::SourceNotSeekable,
        });
    let media_path = std::path::PathBuf::from("/tmp/live-stream.webm");
    let prepared_media = PreparedMedia::from_local_file(media_path, Box::new(demuxer));
    session.load_prepared_media_with_autoplay(prepared_media, false);
    let _ = session.take_events();

    let seek_request = SeekRequest::absolute(MediaTime::from_secs(6));
    session
        .dispatch_command(PlayerCommand::Seek(seek_request))
        .unwrap();

    assert!(session.seek_commit().is_none());
    assert_eq!(
        *seek_log
            .lock()
            .expect("seek log mutex should not be poisoned"),
        Vec::<Duration>::new()
    );
    assert_eq!(session.snapshot().playback_state, PlaybackState::Paused);
    let seek_error = session
        .snapshot()
        .last_error
        .as_ref()
        .expect("blocked seek должен опубликовать user-facing error");
    assert_eq!(seek_error.kind, PlayerErrorKind::SeekUnavailable);
    assert!(seek_error.message.contains("timeline не seekable"));

    let events = session.take_events();
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::SeekRequested(request) if *request == seek_request
        )
    }));
    assert!(events.iter().any(|event| {
        matches!(
            event,
            PlayerEvent::RecoverableError(error)
                if error.kind == PlayerErrorKind::SeekUnavailable
        )
    }));
}

#[test]
fn audio_only_play_and_tick_do_not_require_video_decoder() {
    let mut session = PlayerSession::new();
    let tracks = vec![fake_track(2, TrackKind::Audio)];
    let seek_log = Arc::new(Mutex::new(Vec::new()));
    let mut demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::clone(&seek_log),
    );
    let audio_time_base = media_core::TimeBase::new(1, 48_000).expect("valid audio time base");
    demuxer.packets.push_back(
        media_core::Packet::new_unbounded(
            TrackId::new(2),
            TrackKind::Audio,
            Duration::from_millis(10),
            None,
            false,
            Bytes::from_static(b"audio-packet"),
        )
        .with_track_timestamps(
            Some(media_core::TrackTimestamp::new(
                TrackId::new(2),
                480,
                audio_time_base,
            )),
            Some(media_core::TrackTimestamp::new(
                TrackId::new(2),
                480,
                audio_time_base,
            )),
        ),
    );
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks);
    session.pipeline.select_audio_track(TrackId::new(2));
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.set_playback_state(PlaybackState::Paused);

    session.dispatch_command(PlayerCommand::Play).unwrap();
    assert_eq!(session.snapshot().playback_state, PlaybackState::Playing);

    let tick_result = session.tick(PlayerTickContext::with_config(
        Instant::now(),
        PlayerTickConfig {
            max_demux_packets_per_tick: 1,
            ..PlayerTickConfig::default()
        },
    ));

    assert!(!session.pipeline.has_active_video_decoder());
    assert!(
        matches!(
            session.snapshot().playback_state,
            PlaybackState::Playing | PlaybackState::Draining | PlaybackState::Ended
        ),
        "audio-only tick должен сохранить playback lifecycle без video decoder, actual state: {:?}",
        session.snapshot().playback_state
    );
    assert_eq!(tick_result.demuxed_packets.len(), 1);
    assert_eq!(tick_result.video_frames_presented, 0);
    assert!(tick_result.dropped_video_frames.is_empty());
    assert!(session.snapshot().last_error.is_none());
    assert_eq!(
        tick_result.demuxed_packets[0]
            .track_pts
            .expect("tick telemetry должен сохранить raw PTS")
            .units
            .get(),
        480
    );
    assert_eq!(
        tick_result.demuxed_packets[0]
            .track_dts
            .expect("tick telemetry должен сохранить raw DTS")
            .units
            .get(),
        480
    );
}
