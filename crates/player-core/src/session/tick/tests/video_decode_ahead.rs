//! Политика time-based decode-ahead: пока первый кадр preroll-а не получен, окно
//! опережения audio clock не держит video packets; после первого кадра действует как раньше.
//!
//! Проверяем наблюдаемый результат: сколько packets реально ушло в decoder при разных
//! состояниях session (audio clock стоит на 0, packets идут с шагом 100 мс).

use super::super::video_decoder_io::{
    VideoDecoderDecodeAheadLimits, can_send_video_packet_to_decoder,
};
use super::*;

/// Сколько packets выстроено в очередь (шаг 100 мс => 1.1 с вперёд, больше окна 500 мс).
const QUEUED_PACKETS: usize = 12;

/// Сколько packets за проход пропускает presentation-queue admission (нормальный лимит,
/// который остаётся, даже когда временное окно снято): `max_video_present_queue`.
fn present_admission_budget() -> usize {
    PlayerTickConfig::default().max_video_present_queue
}

/// Окно decode-ahead в тестах (дефолт `video.max_decode_ahead_ms`), мс.
const WINDOW_MS: u64 = 500;

/// Сколько packets допускает окно при audio clock = 0: pts 0, 100, ..., 500 включительно.
const PACKETS_INSIDE_WINDOW: usize = 6;

/// Session с audio/video треками, audio clock на 0 и очередью pending video packets.
fn session_with_queued_video(select_audio: bool) -> (PlayerSession, RecordingVideoDecoderThread) {
    let decoder_thread = RecordingVideoDecoderThread::new();
    let mut session = PlayerSession::new();
    install_empty_demuxer(&mut session);
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::H264),
    );
    if select_audio {
        session.pipeline.select_audio_track(TrackId::new(2));
        install_fixed_audio_output(&mut session, 0.0);
        // Audio clock стоит на 0: так выглядит startup, пока audio stream не запущен.
        session
            .pipeline
            .install_audio_clock(Arc::new(FixedAudioClock) as Arc<dyn PlayerAudioClock>);
    }
    for index in 0..QUEUED_PACKETS {
        enqueue_selected_video_packet(
            &mut session,
            Duration::from_millis(index as u64 * 100),
            Bytes::from_static(b"vp8-frame"),
            if index == 0 {
                PacketKeyframe::Keyframe
            } else {
                PacketKeyframe::NotKeyframe
            },
        );
    }
    (session, decoder_thread)
}

/// Прогоняет один send-pass с дефолтным окном и возвращает число ушедших packets.
fn send_pass(session: &mut PlayerSession) -> usize {
    let limits = video_decoder_io_limits(
        &PlayerTickConfig::default(),
        0,
        QUEUED_PACKETS,
        Duration::from_millis(WINDOW_MS),
    );
    send_pending_video_packets_to_decoder(session, &mut PlayerTickResult::default(), limits, None)
}

/// Без исправления здесь было бы `PACKETS_INSIDE_WINDOW`: decoder не получал глубину для кадра.
#[test]
fn buffering_without_first_frame_does_not_hold_packets_by_time_window() {
    let (mut session, decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Buffering);
    assert!(session.video_first_frame_is_awaited());

    let sent = send_pass(&mut session);

    // Окно (6 packets) снято, но packets остаются ограниченными нормальным admission.
    assert_eq!(sent, present_admission_budget());
    assert!(sent > PACKETS_INSIDE_WINDOW);
    assert_eq!(decoder_thread.sent_packets().len(), sent);
    assert_eq!(
        session.pipeline.pending_video_packet_len(),
        QUEUED_PACKETS - sent
    );
}

/// Остальные лимиты остаются: ограничение числа packets за проход работает и без окна.
#[test]
fn waived_window_still_respects_per_pass_packet_limit() {
    let (mut session, decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Buffering);
    let limits = video_decoder_io_limits(
        &PlayerTickConfig::default(),
        0,
        3,
        Duration::from_millis(WINDOW_MS),
    );

    let sent = send_pending_video_packets_to_decoder(
        &mut session,
        &mut PlayerTickResult::default(),
        limits,
        None,
    );

    assert_eq!(sent, 3);
    assert_eq!(decoder_thread.sent_packets().len(), 3);
    assert_eq!(
        session.pipeline.pending_video_packet_len(),
        QUEUED_PACKETS - 3
    );
}

/// Backpressure decoder-а действует и пока окно снято (ограниченная память).
#[test]
fn waived_window_still_respects_decoder_backpressure() {
    let (mut session, decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Buffering);
    decoder_thread.push_send_result(Err(DecodeSendError::Backpressure(
        DecodeBackpressureReason::PacketQueueFull {
            queued_packets: 1,
            capacity: 1,
        },
    )));

    let sent = send_pass(&mut session);

    assert_eq!(sent, 0);
    assert_eq!(session.pipeline.pending_video_packet_len(), QUEUED_PACKETS);
}

/// После первого кадра окно снова держит packets, опережающие audio clock.
#[test]
fn buffering_with_first_frame_applies_time_window_again() {
    let (mut session, decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Buffering);
    session
        .pipeline
        .set_present_video_frame(decoded_frame(Duration::ZERO, 1));
    assert!(!session.video_first_frame_is_awaited());

    let sent = send_pass(&mut session);

    assert_eq!(sent, PACKETS_INSIDE_WINDOW);
    assert_eq!(decoder_thread.sent_packets().len(), PACKETS_INSIDE_WINDOW);
}

/// Steady-state Playing без кадра в present slot: окно действует как раньше.
#[test]
fn playing_applies_time_window() {
    let (mut session, _decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Playing);
    assert!(!session.video_first_frame_is_awaited());

    assert_eq!(send_pass(&mut session), PACKETS_INSIDE_WINDOW);
}

/// Seek имеет собственный preroll: состояние `Seeking` не снимает окно.
#[test]
fn seeking_keeps_time_window() {
    let (mut session, _decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Seeking);
    assert!(!session.video_first_frame_is_awaited());

    assert_eq!(send_pass(&mut session), PACKETS_INSIDE_WINDOW);
}

/// Без выбранного аудио окно не применялось и раньше: поведение не меняется.
#[test]
fn media_without_audio_is_unaffected() {
    for state in [PlaybackState::Buffering, PlaybackState::Playing] {
        let (mut session, _decoder_thread) = session_with_queued_video(false);
        session.set_playback_state(state);

        assert_eq!(
            send_pass(&mut session),
            present_admission_budget(),
            "{state:?}"
        );
    }
}

/// Короткий файл: EOF пришёл раньше первого кадра, старт ждёт кадр внутри `Draining`.
#[test]
fn startup_waiting_for_first_frame_in_eof_drain_waives_window() {
    let (mut session, _decoder_thread) = session_with_queued_video(true);
    session.mark_startup_autoplay_preroll_pending();
    session.set_playback_state(PlaybackState::Draining);
    assert!(session.video_first_frame_is_awaited());

    assert_eq!(send_pass(&mut session), present_admission_budget());
}

/// Обычный EOF drain после состоявшегося старта окно не снимает.
#[test]
fn ordinary_eof_drain_keeps_window() {
    let (mut session, _decoder_thread) = session_with_queued_video(true);
    session.set_playback_state(PlaybackState::Draining);
    assert!(!session.video_first_frame_is_awaited());

    assert_eq!(send_pass(&mut session), PACKETS_INSIDE_WINDOW);
}

/// Boundary-предикат сам по себе: окно/лимиты и снятие окна не меняют состояние session.
#[test]
fn gate_decision_does_not_mutate_session_state() {
    let (session, decoder_thread) = session_with_queued_video(true);
    let limits = VideoDecoderDecodeAheadLimits::new(
        Duration::from_millis(WINDOW_MS),
        Duration::from_millis(WINDOW_MS),
    );
    let far_packet = Duration::from_secs(10);

    let verdict = |session: &PlayerSession| {
        can_send_video_packet_to_decoder(
            session,
            video_decoder_texture_limits(&PlayerTickConfig::default()),
            limits,
            host_upload_ready_queue_capacity(&PlayerTickConfig::default()),
            false,
            far_packet,
        )
    };
    assert!(!verdict(&session), "Paused/idle session keeps the window");

    assert_eq!(session.pipeline.pending_video_packet_len(), QUEUED_PACKETS);
    assert!(decoder_thread.sent_packets().is_empty());
}
