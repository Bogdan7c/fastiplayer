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

/// Пустая очередь: удалять нечего, учёт не меняется.
#[test]
fn discarding_superseded_audio_from_an_empty_queue_is_a_no_op() {
    let mut pipeline = PlaybackPipeline::default();

    assert_eq!(
        pipeline.discard_pending_audio_superseded_by_target(Duration::from_secs(8)),
        0
    );
    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog::default()
    );
}

/// Packets целиком до цели удаляются вместе с учётом байт; packet с целью внутри — нет.
#[test]
fn discarding_superseded_audio_keeps_the_packet_that_contains_the_target() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.select_audio_track(TrackId::new(2));
    for pts_ms in [8_000, 8_020, 8_040, 8_060, 8_080] {
        pipeline.enqueue_pending_audio_packet(audio_packet(pts_ms, 10));
    }

    // Цель 8,05 с лежит внутри packet-а 8,04 с.
    let discarded =
        pipeline.discard_pending_audio_superseded_by_target(Duration::from_millis(8_050));

    assert_eq!(discarded, 2);
    assert_eq!(
        pipeline.front_pending_audio_packet_start(),
        Some(Duration::from_millis(8_040))
    );
    assert_eq!(
        pipeline.pending_audio_backlog(),
        PendingAudioBacklog {
            packets: 3,
            encoded_bytes: 30,
            media_span: Duration::from_millis(40),
        }
    );
}

/// Последний packet очереди никогда не удаляется: его конец ещё не доказан.
#[test]
fn discarding_superseded_audio_never_removes_the_last_packet() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(8_000, 10));
    pipeline.enqueue_pending_audio_packet(audio_packet(8_020, 10));

    let discarded = pipeline.discard_pending_audio_superseded_by_target(Duration::from_secs(60));

    assert_eq!(discarded, 1);
    assert_eq!(pipeline.pending_audio_backlog().packets, 1);
}

/// Битые PTS (packet не по порядку) — доказательства конца нет, ничего не удаляется.
#[test]
fn discarding_superseded_audio_stops_at_out_of_order_timestamps() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(9_000, 10));
    pipeline.enqueue_pending_audio_packet(audio_packet(8_000, 10));

    let discarded =
        pipeline.discard_pending_audio_superseded_by_target(Duration::from_millis(8_500));

    assert_eq!(discarded, 0);
    assert_eq!(pipeline.pending_audio_backlog().packets, 2);
    assert_eq!(pipeline.pending_audio_backlog().encoded_bytes, 20);
}

/// Удаление сжатого звука не трогает видео, output и поколение.
#[test]
fn discarding_superseded_audio_does_not_touch_other_pipeline_state() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.enqueue_pending_audio_packet(audio_packet(8_000, 10));
    pipeline.enqueue_pending_audio_packet(audio_packet(8_020, 10));
    let generation_before = pipeline.seek_generation();
    let base_before = pipeline.media_clock_base();

    let _ = pipeline.discard_pending_audio_superseded_by_target(Duration::from_millis(8_030));

    assert_eq!(pipeline.seek_generation(), generation_before);
    assert_eq!(pipeline.media_clock_base(), base_before);
    assert_eq!(pipeline.pending_video_packet_len(), 0);
    assert!(!pipeline.has_audio_output());
}
