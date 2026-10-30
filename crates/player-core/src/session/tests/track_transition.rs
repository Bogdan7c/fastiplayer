//! Смена описания дорожек посреди потока без сброса («эстафета», сессия 03d моста).
//!
//! Подставной decoder записывает всё, что до него дошло: packet-ы, настройки,
//! выжимания и flush. Поэтому тесты видят порядок передачи decoder-а по шагам.

use super::test_support::*;
use super::*;

const VIDEO_TRACK: TrackId = TrackId::new(1);

/// Хватает, чтобы за один tick прочитать весь сценарий и отправить все packet-ы.
fn handoff_tick_config() -> PlayerTickConfig {
    PlayerTickConfig {
        max_demux_packets_per_tick: 16,
        max_video_packets_sent_per_tick: 16,
        ..seek_admission_tick_config(8, 8)
    }
}

fn tick(session: &mut PlayerSession) -> PlayerTickResult {
    session.tick(PlayerTickContext::with_config(
        Instant::now(),
        handoff_tick_config(),
    ))
}

fn video_track() -> TrackInfo {
    fake_track(VIDEO_TRACK.get(), TrackKind::Video)
}

/// То же видео с другими SPS/PPS: так выглядит смена качества у MSE и HLS/DASH.
fn video_track_with_new_parameter_sets() -> TrackInfo {
    let mut track = video_track();
    track.codec_private = Some(Bytes::from_static(b"next quality parameter sets"));
    track
}

fn video_packet(pts_ms: u64, keyframe: PacketKeyframe) -> DemuxReadEvent {
    DemuxReadEvent::Packet(fake_video_packet_with_keyframe(
        VIDEO_TRACK,
        Duration::from_millis(pts_ms),
        keyframe,
    ))
}

fn tracks_changed(tracks: Vec<TrackInfo>) -> DemuxReadEvent {
    DemuxReadEvent::TracksChanged(DemuxTrackListUpdate::new(
        tracks,
        Some(Duration::from_secs(30)),
    ))
}

/// Играющая session с подставным decoder-ом и demuxer-ом, отдающим `events` по порядку.
fn playing_session_with_events(
    events: Vec<DemuxReadEvent>,
) -> (PlayerSession, SharedFakeVideoDecoderThread) {
    let mut session = PlayerSession::new();
    let decoder = SharedFakeVideoDecoderThread::new();
    session.pipeline.set_video_decoder_thread(decoder.clone());
    let tracks = vec![video_track()];
    let mut demuxer = FakeDemuxer::new(
        tracks.clone(),
        Some(Duration::from_secs(30)),
        Arc::new(Mutex::new(Vec::new())),
    );
    for event in events {
        demuxer.push_event(event);
    }
    session
        .pipeline
        .install_opened_media(Box::new(demuxer), None, None, tracks.clone());
    session
        .select_default_video_track(&tracks, "fake media lacking video track")
        .expect("fake video track is accepted");
    session.set_snapshot_duration(Some(Duration::from_secs(30)));
    session.set_playback_state(PlaybackState::Playing);
    (session, decoder)
}

fn sent_pts(decoder: &SharedFakeVideoDecoderThread) -> Vec<Duration> {
    decoder
        .sent_packets()
        .iter()
        .map(|packet| packet.pts)
        .collect()
}

fn millis(values: &[u64]) -> Vec<Duration> {
    values.iter().copied().map(Duration::from_millis).collect()
}

/// Сценарий смены качества: два packet-а старого описания, смена, два нового.
fn quality_switch_events() -> Vec<DemuxReadEvent> {
    vec![
        video_packet(0, PacketKeyframe::Keyframe),
        video_packet(40, PacketKeyframe::NotKeyframe),
        tracks_changed(vec![video_track_with_new_parameter_sets()]),
        video_packet(80, PacketKeyframe::Keyframe),
        video_packet(120, PacketKeyframe::NotKeyframe),
    ]
}

/// Decoder отдал кадры двух старых packet-ов и подтвердил их обработку.
fn decode_old_episode(session: &PlayerSession, decoder: &SharedFakeVideoDecoderThread) {
    for (pts_ms, handle) in [(0, 1), (40, 2)] {
        decoder.push_decoded_frame(decoded_frame_for_current_seek_generation(
            session,
            Duration::from_millis(pts_ms),
            handle,
        ));
    }
    decoder.add_completed_packet_count(2);
}

/// Главный сценарий: старые кадры доигрываются, decoder передаётся на границе.
#[test]
fn quality_change_hands_the_decoder_over_at_the_boundary_without_a_reset() {
    let (mut session, decoder) = playing_session_with_events(quality_switch_events());
    let generation = session.pipeline.seek_generation();

    // Tick 1: старый эпизод уходит в decoder, смена принята без сброса.
    tick(&mut session);
    assert_eq!(sent_pts(&decoder), millis(&[0, 40]));
    assert_eq!(session.pipeline.seek_generation(), generation, "no reset");
    assert_eq!(decoder.flush_count(), 0);
    assert_eq!(
        session.pipeline.tracks()[0].codec_private,
        video_track_with_new_parameter_sets().codec_private,
        "UI sees the new description right away"
    );

    // Tick 2: новые packet-ы прочитаны, но ждут: старый decoder ещё работает.
    tick(&mut session);
    assert_eq!(sent_pts(&decoder), millis(&[0, 40]));
    assert!(decoder.eof_drain_requests().is_empty());
    assert_eq!(session.pipeline.pending_video_packet_len(), 2);

    // Decoder доделал старые packet-ы: их кадры обязаны дойти до очереди показа.
    decode_old_episode(&session, &decoder);
    // Tick 3: ACK-и забраны в начале шага передачи — decoder сразу выжимается,
    // а кадры старого эпизода уходят в очередь показа.
    let result = tick(&mut session);
    assert!(
        result
            .dropped_video_frames
            .iter()
            .all(|drop| drop.reason != crate::PlayerVideoDropReason::StaleGeneration),
        "old frames are not thrown away"
    );
    assert!(decoder.released_handles().is_empty());
    assert_eq!(
        decoder.eof_drain_requests(),
        vec![generation],
        "the old decoder is drained once, in the same generation"
    );
    assert_eq!(
        decoder.configured_streams().len(),
        1,
        "not reconfigured yet"
    );
    assert_eq!(sent_pts(&decoder), millis(&[0, 40]));

    // Tick: decoder выжат — flush, настройка на новое описание, новые packet-ы.
    tick(&mut session);
    assert_eq!(decoder.flush_count(), 1);
    let configured = decoder.configured_streams();
    assert_eq!(configured.len(), 2);
    assert_eq!(
        configured[1].codec_private,
        video_track_with_new_parameter_sets().codec_private
    );
    assert_eq!(sent_pts(&decoder), millis(&[0, 40, 80, 120]));
    assert_eq!(session.pipeline.seek_generation(), generation);
    assert!(session.snapshot().last_error.is_none());
}

/// После передачи настоящий конец потока снова выжимает decoder: flush вернул
/// его в «не выжат», и хвост последнего эпизода не теряется.
#[test]
fn end_of_stream_after_a_handoff_drains_the_new_decoder_again() {
    let (mut session, decoder) = playing_session_with_events(quality_switch_events());
    let generation = session.pipeline.seek_generation();
    tick(&mut session);
    tick(&mut session);
    decode_old_episode(&session, &decoder);
    for _ in 0..3 {
        tick(&mut session);
    }
    assert_eq!(sent_pts(&decoder), millis(&[0, 40, 80, 120]));

    // Новые packet-ы обработаны; demuxer уже сообщил конец потока.
    decoder.add_completed_packet_count(2);
    for _ in 0..3 {
        tick(&mut session);
    }

    assert_eq!(
        decoder.eof_drain_requests(),
        vec![generation, generation],
        "handoff drain and the real end-of-stream drain both reach the decoder"
    );
}

/// Описание decoder-а не менялось (длительность, метки): ни выжимания, ни flush.
#[test]
fn unchanged_decoder_description_updates_tracks_without_touching_the_decoder() {
    let mut relabeled = video_track();
    relabeled.labels.title = Some("Main".to_owned());
    let (mut session, decoder) = playing_session_with_events(vec![
        video_packet(0, PacketKeyframe::Keyframe),
        tracks_changed(vec![relabeled]),
        video_packet(40, PacketKeyframe::NotKeyframe),
    ]);

    tick(&mut session);
    tick(&mut session);

    assert_eq!(sent_pts(&decoder), millis(&[0, 40]));
    assert!(decoder.eof_drain_requests().is_empty());
    assert_eq!(decoder.flush_count(), 0);
    assert_eq!(decoder.configured_streams().len(), 1);
    assert_eq!(
        session.pipeline.tracks()[0].labels.title.as_deref(),
        Some("Main")
    );
}

/// Сменился состав дорожек: прежний полный сброс со своей новой generation.
#[test]
fn track_topology_change_still_takes_the_full_reset() {
    let mut replaced = video_track();
    replaced.id = TrackId::new(3);
    let (mut session, decoder) = playing_session_with_events(vec![
        video_packet(0, PacketKeyframe::Keyframe),
        tracks_changed(vec![replaced]),
    ]);
    let generation = session.pipeline.seek_generation();

    tick(&mut session);

    assert_ne!(session.pipeline.seek_generation(), generation);
    assert_eq!(decoder.flush_count(), 1);
}

/// Во время перемотки у `TracksChanged` свои правила: эстафета не вмешивается.
#[test]
fn description_change_during_an_active_seek_keeps_the_seek_reset_rules() {
    let mut session = PlayerSession::new();
    install_fake_media(&mut session, vec![video_track()]);
    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .expect("seek accepted");
    let generation = session.pipeline.seek_generation();

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![video_track_with_new_parameter_sets()],
        Some(Duration::from_secs(30)),
    ));

    assert_ne!(session.pipeline.seek_generation(), generation);
    assert_eq!(
        session.seek_commit().expect("seek stays active").generation,
        session.pipeline.seek_generation()
    );
}

/// Активный decoder не примет новое описание — прежний сброс со своей ошибкой.
#[test]
fn description_the_active_decoder_rejects_takes_the_full_reset() {
    let mut unsupported = video_track();
    unsupported.codec_id = "V_UNKNOWN_CODEC".to_owned();
    let (mut session, decoder) = playing_session_with_events(vec![
        video_packet(0, PacketKeyframe::Keyframe),
        tracks_changed(vec![unsupported]),
    ]);
    let generation = session.pipeline.seek_generation();

    tick(&mut session);

    assert_ne!(session.pipeline.seek_generation(), generation);
    assert_eq!(decoder.flush_count(), 1);
    assert!(session.snapshot().last_error.is_some());
}

/// Перемотка посреди передачи: flush уже выбросил старое, поэтому decoder сразу
/// получает последнее описание, без выжимания и без ожидания границы.
#[test]
fn seek_during_a_pending_handoff_configures_the_latest_description_after_flush() {
    let (mut session, decoder) = playing_session_with_events(quality_switch_events());
    tick(&mut session);
    tick(&mut session);
    assert!(session.pipeline.pending_video_track_handoff().is_some());

    session
        .dispatch_command(PlayerCommand::Seek(SeekRequest::absolute(
            MediaTime::from_secs(6),
        )))
        .expect("seek accepted");

    let configured = decoder.configured_streams();
    assert_eq!(
        configured
            .last()
            .and_then(|config| config.codec_private.clone()),
        video_track_with_new_parameter_sets().codec_private
    );
    assert!(decoder.eof_drain_requests().is_empty());
    assert!(session.pipeline.pending_video_track_handoff().is_none());
}

/// Новый decoder не настроился — ошибка видна, а не тихая порча кадров.
#[test]
fn failed_reconfiguration_at_the_boundary_is_a_fatal_error() {
    let (mut session, decoder) = playing_session_with_events(quality_switch_events());
    tick(&mut session);
    tick(&mut session);
    decode_old_episode(&session, &decoder);
    decoder.push_configure_result(video_core::VideoStreamConfigResult::Fatal(
        DecodeThreadError::new("scripted configure failure"),
    ));
    for _ in 0..3 {
        tick(&mut session);
    }

    let error = session
        .snapshot()
        .last_error
        .clone()
        .expect("handoff failure is reported");
    assert!(
        error.message.contains("scripted configure failure"),
        "{error:?}"
    );
    assert_eq!(
        sent_pts(&decoder),
        millis(&[0, 40]),
        "new packets never reach a broken decoder"
    );
}

/// Смена кодека звука: decoder меняется ровно на первом packet-е нового описания,
/// выход звука и его часы не пересоздаются.
#[test]
fn audio_codec_change_swaps_the_decoder_at_the_first_packet_of_the_new_description() {
    let (decoder_factory, decoder_factory_handle) = ScriptedAudioDecoderFactory::success();
    let mut session = PlayerSession::with_audio_decoder_factory(decoder_factory);
    let audio_track = fake_audio_track_with_codec(2, "A_AAC");
    install_fake_media(&mut session, vec![audio_track.clone()]);
    session.init_audio_pipeline(std::slice::from_ref(&audio_track));
    let output = install_ready_audio_runtime(&mut session, 0.0, None);
    let generation = session.pipeline.seek_generation();
    let old_packet = |pts_ms| {
        PendingAudioPacket::new_unbounded(
            TrackId::new(2),
            Duration::from_millis(pts_ms),
            None,
            None,
            generation,
            Bytes::from_static(b"aac"),
        )
    };

    session.handle_demux_track_list_update(DemuxTrackListUpdate::new(
        vec![fake_audio_track_with_codec(2, "A_OPUS")],
        Some(Duration::from_secs(30)),
    ));
    let next_description = session.pipeline.admitted_audio_track_description();

    // Packet старого описания разобран раньше смены: его декодирует старый decoder.
    session.pipeline.enqueue_pending_audio_packet(old_packet(0));
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);
    assert!(decoder_factory_handle.created_configs().is_empty());

    session
        .pipeline
        .enqueue_pending_audio_packet(old_packet(20).with_track_description(next_description));
    session.process_pending_audio_packets_with_buffer_limit(1_000.0);

    let created = decoder_factory_handle.created_configs();
    assert_eq!(created.len(), 1);
    assert_eq!(created[0].codec_id(), "A_OPUS");
    assert!(
        session.pipeline.has_audio_output(),
        "output survives the switch"
    );
    assert_eq!(
        output.pause_count(),
        0,
        "the output is not paused or restarted"
    );
    assert_eq!(session.pipeline.seek_generation(), generation);
}

/// Канал управления decoder-а на миг полон: передача повторяется, а не роняет фильм.
#[test]
fn busy_decoder_control_channel_at_the_boundary_retries_the_handoff() {
    let (mut session, decoder) = playing_session_with_events(quality_switch_events());
    tick(&mut session);
    tick(&mut session);
    decode_old_episode(&session, &decoder);
    decoder.push_configure_result(video_core::VideoStreamConfigResult::Backpressure(
        video_core::VideoDecoderControlBackpressureReason::ControlChannelFull {
            queued_messages: 8,
            capacity: 8,
        },
    ));
    for _ in 0..6 {
        tick(&mut session);
    }

    assert!(
        session.snapshot().last_error.is_none(),
        "{:?}",
        session.snapshot().last_error
    );
    let configured = decoder.configured_streams();
    assert_eq!(
        configured
            .last()
            .and_then(|config| config.codec_private.clone()),
        video_track_with_new_parameter_sets().codec_private,
        "the retry configured the new description"
    );
    assert_eq!(sent_pts(&decoder), millis(&[0, 40, 80, 120]));
}
