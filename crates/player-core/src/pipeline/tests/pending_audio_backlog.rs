//! Учёт объёма сжатого звука в очереди pending audio (сессия 03e моста).
//!
//! Session ограничивает этот запас, пока preroll ждёт первый кадр, поэтому счётчик
//! байт и media-длительность обязаны совпадать с содержимым очереди после любых
//! изменений через boundary pipeline-а.

use super::*;

/// Audio packet выбранной дорожки с заданными PTS и размером payload-а.
fn audio_packet(pts_ms: u64, payload_bytes: usize) -> PendingAudioPacket {
    PendingAudioPacket::new_unbounded(
        TrackId::new(2),
        Duration::from_millis(pts_ms),
        None,
        None,
        0,
        Bytes::from(vec![0_u8; payload_bytes]),
    )
}

#[test]
fn empty_queue_reports_no_backlog() {
    let pipeline = PlaybackPipeline::default();

    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog::default()
    );
}

#[test]
fn backlog_follows_every_queue_change() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(1_000, 10));
    pipeline.enqueue_pending_audio_packet(audio_packet(1_020, 20));
    pipeline.enqueue_pending_audio_packet(audio_packet(1_040, 30));

    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog {
            packets: 3,
            encoded_bytes: 60,
            media_span: Duration::from_millis(40),
        }
    );

    // Decode забрал первый packet.
    let decoded = pipeline
        .pop_pending_audio_packet_front()
        .expect("queued packet");
    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog {
            packets: 2,
            encoded_bytes: 50,
            media_span: Duration::from_millis(20),
        }
    );

    // Throttle вернул его обратно — объём тот же, что до извлечения.
    pipeline.push_pending_audio_packet_front(decoded);
    assert_eq!(pipeline.pending_audio_backlog().encoded_bytes, 60);
    assert_eq!(
        pipeline.pending_audio_backlog().media_span,
        Duration::from_millis(40)
    );

    pipeline.clear_pending_audio_packets();
    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog::default()
    );
    assert!(pipeline.pop_pending_audio_packet_front().is_none());
}

/// Packets не по порядку (битые PTS) дают нулевую длительность, а не переполнение;
/// объём в байтах при этом считается честно.
#[test]
fn out_of_order_timestamps_give_zero_span_but_count_bytes() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(5_000, 7));
    pipeline.enqueue_pending_audio_packet(audio_packet(1_000, 9));

    let backlog = pipeline.pending_audio_backlog();
    assert_eq!(backlog.media_span, Duration::ZERO);
    assert_eq!(backlog.encoded_bytes, 16);
    assert_eq!(backlog.packets, 2);
}

/// Сброс media очищает очередь вместе со счётчиком.
#[test]
fn media_reset_clears_backlog_accounting() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(0, 100));

    pipeline.reset_media_slots();

    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog::default()
    );
}
