use media_core::{
    ExactPresentationWindow, PacketPresentationWindow, TimeBase, TrackId, TrackTimestamp,
};

use super::*;

/// Строит exact audio window для lifecycle-тестов pending queue.
fn bounded_audio_window(track_id: TrackId) -> PacketPresentationWindow {
    let time_base = TimeBase::new(1, 48_000).expect("test time base должна быть валидной");
    PacketPresentationWindow::Bounded(
        ExactPresentationWindow::new(
            TrackTimestamp::new(track_id, 480, time_base),
            TrackTimestamp::new(track_id, 1_440, time_base),
        )
        .expect("test presentation window должно быть валидным"),
    )
}

#[test]
fn pending_audio_packet_unbounded_constructor_is_explicit() {
    let packet = PendingAudioPacket::new_unbounded(
        TrackId::new(2),
        Duration::from_millis(10),
        None,
        None,
        0,
        Bytes::from_static(b"audio"),
    );

    assert_eq!(
        packet.presentation_window(),
        PacketPresentationWindow::Unbounded
    );
}

#[test]
fn throttled_requeue_preserves_exact_audio_packet_window() {
    let mut pipeline = PlaybackPipeline::default();
    let track_id = TrackId::new(2);
    let bounded_window = bounded_audio_window(track_id);
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_with_presentation_window(
        track_id,
        Duration::from_millis(10),
        bounded_window,
        pipeline.seek_generation(),
        Bytes::from_static(b"audio"),
    ));

    let throttled_packet = pipeline
        .pop_pending_audio_packet_front()
        .expect("throttle path должен забрать front packet");
    assert_eq!(throttled_packet.presentation_window(), bounded_window);

    pipeline.push_pending_audio_packet_front(throttled_packet);

    let requeued_packet = pipeline
        .pop_pending_audio_packet_front()
        .expect("throttle path должен вернуть тот же packet");
    assert_eq!(requeued_packet.presentation_window(), bounded_window);
    assert_eq!(requeued_packet.encoded_bytes(), b"audio");
}

#[test]
fn seek_clear_drops_bounded_audio_packet_and_window_together() {
    let mut pipeline = PlaybackPipeline::default();
    let track_id = TrackId::new(2);
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_with_presentation_window(
        track_id,
        Duration::from_millis(10),
        bounded_audio_window(track_id),
        pipeline.seek_generation(),
        Bytes::from_static(b"audio"),
    ));

    pipeline.clear_pending_packets_for_seek();

    assert!(pipeline.pending_audio_packet_is_empty());
}
