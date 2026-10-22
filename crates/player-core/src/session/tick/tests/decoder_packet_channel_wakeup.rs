//! Планирование wakeup-а, когда decoder send channel полон при обычном воспроизведении.
//!
//! Регрессия cpu-parity сессии 08: software decoder 4K AV1 не успевал за 60 fps,
//! bounded packet channel decoder-а стоял полным, а planner считал pending packet
//! «готовой работой» и ставил zero-delay `PipelineWorkReady`. Tick снова получал
//! `PacketQueueFull` и сразу просыпался — ~175 тыс. пустых тиков в секунду.
//! Проверка полного channel-а раньше действовала только в Accurate seek preroll-е.

use super::*;

/// Production default интервала decoder readiness poll-а.
const DECODER_READINESS_POLL_INTERVAL: Duration = Duration::from_millis(2);

/// Резервный coarse interval worker-а (production default — 250 мс).
const COARSE_WAKEUP_INTERVAL: Duration = Duration::from_millis(250);

/// Ёмкость decoder send channel-а в сценарии (`decoder_packet_channel_frames`).
const PACKET_CHANNEL_CAPACITY: usize = 4;

/// Tick config, у которого граница channel-а совпадает с fake decoder-ом.
fn tick_config_with_packet_channel_capacity() -> PlayerTickConfig {
    PlayerTickConfig {
        max_pending_video_packets: PACKET_CHANNEL_CAPACITY,
        ..PlayerTickConfig::default()
    }
}

/// Playing session с выбранным video track-ом, пустой очередью показа (decoder
/// не успевает) и одним pending packet-ом; decoder channel держит `queued_packets`.
fn playing_session_with_decoder_channel(
    queued_packets: usize,
    activity_snapshot: video_core::VideoDecoderActivitySnapshot,
) -> (PlayerSession, RecordingVideoDecoderThread) {
    let decoder_thread = RecordingVideoDecoderThread::new_with_activity_snapshot(activity_snapshot)
        .with_packet_queue_depth(queued_packets);
    let mut session = PlayerSession::new();
    session.set_playback_state(PlaybackState::Playing);
    session
        .pipeline
        .set_video_decoder_thread(decoder_thread.clone());
    session.pipeline.select_video_track(
        TrackId::new(1),
        VideoDecodeRequirement::new(VideoCodec::Av1),
    );
    enqueue_selected_video_packet(
        &mut session,
        Duration::from_millis(120),
        Bytes::from_static(b"video-packet-waiting-for-decoder-channel"),
        true,
    );
    (session, decoder_thread)
}

#[test]
fn full_decoder_channel_during_playback_is_not_immediate_video_work() {
    let (_notifier, subscription) = video_core::VideoDecoderActivityNotifier::new();
    let tick_config = tick_config_with_packet_channel_capacity();
    let (session, _decoder_thread) =
        playing_session_with_decoder_channel(PACKET_CHANNEL_CAPACITY, subscription.snapshot());

    assert!(!pending_video_work_available(&session, &tick_config));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        DECODER_READINESS_POLL_INTERVAL,
        COARSE_WAKEUP_INTERVAL,
    );

    // Освободить channel может только decoder (ACK packet-а будит worker через
    // activity notifier); zero-delay retry здесь — холостой круг.
    assert_eq!(plan.reason, WorkerWakeupReason::DecodeReadiness);
    assert_ne!(plan.delay, Some(Duration::ZERO));
}

#[test]
fn decoder_channel_with_room_keeps_immediate_video_work() {
    let (_notifier, subscription) = video_core::VideoDecoderActivityNotifier::new();
    let tick_config = tick_config_with_packet_channel_capacity();
    let (session, _decoder_thread) =
        playing_session_with_decoder_channel(PACKET_CHANNEL_CAPACITY - 1, subscription.snapshot());

    assert!(pending_video_work_available(&session, &tick_config));

    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        DECODER_READINESS_POLL_INTERVAL,
        COARSE_WAKEUP_INTERVAL,
    );

    assert_eq!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
    assert_eq!(plan.delay, Some(Duration::ZERO));
}

#[test]
fn rejected_send_on_full_channel_is_not_retried_by_a_zero_delay_tick() {
    let (_notifier, subscription) = video_core::VideoDecoderActivityNotifier::new();
    let tick_config = tick_config_with_packet_channel_capacity();
    let (mut session, decoder_thread) =
        playing_session_with_decoder_channel(PACKET_CHANNEL_CAPACITY, subscription.snapshot());
    decoder_thread.push_send_result(Err(DecodeSendError::Backpressure(
        DecodeBackpressureReason::PacketQueueFull {
            queued_packets: PACKET_CHANNEL_CAPACITY,
            capacity: PACKET_CHANNEL_CAPACITY,
        },
    )));
    let mut tick_result = PlayerTickResult::default();

    // Sender упирается в полный channel: packet остаётся у worker-а.
    let sent_packets = send_pending_video_packets_to_decoder(
        &mut session,
        &mut tick_result,
        decoder_io_limits_for_tests(0, 1),
        None,
    );
    assert_eq!(sent_packets, 0);
    assert!(!session.pipeline.pending_video_packet_is_empty());

    // Planner видит то же состояние, что sender: следующий tick не сразу.
    let plan = session.worker_wakeup_plan(
        Instant::now(),
        &tick_config,
        DECODER_READINESS_POLL_INTERVAL,
        COARSE_WAKEUP_INTERVAL,
    );
    assert_ne!(plan.delay, Some(Duration::ZERO));
    assert_ne!(plan.reason, WorkerWakeupReason::PipelineWorkReady);
}
