//! Generation seek-а и очистка очередей без затрагивания выбора/декодера.

use super::*;

#[test]
fn begin_seek_generation_saturates_without_wrapping() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.set_seek_generation_for_tests(u64::MAX - 1);

    assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
    assert_eq!(pipeline.seek_generation(), u64::MAX);
    assert_eq!(pipeline.begin_seek_generation(), u64::MAX);
    assert_eq!(pipeline.seek_generation(), u64::MAX);
    assert!(pipeline.packet_generation_is_current(u64::MAX));
    assert!(!pipeline.packet_generation_is_current(u64::MAX - 1));
}

#[test]
fn clear_pending_packets_for_seek_does_not_touch_selection_or_decoder_state() {
    let mut pipeline = PlaybackPipeline::default();
    let video_track_id = TrackId::new(10);
    let audio_track_id = TrackId::new(20);
    let requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let generation = pipeline.seek_generation();

    pipeline.select_audio_track(audio_track_id);
    pipeline.select_video_track(video_track_id, requirement.clone());
    pipeline.mark_video_decoder_bootstrapped();
    pipeline.note_video_packet_sent_to_decoder();
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        audio_track_id,
        Duration::from_millis(10),
        None,
        None,
        generation,
        Bytes::from_static(b"audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(20),
        generation,
        Bytes::from_static(b"video"),
        true,
    ));

    pipeline.clear_pending_packets_for_seek();

    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());
    assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
    assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
    assert_eq!(pipeline.active_video_requirement(), Some(&requirement));
    assert!(!pipeline.video_decoder_needs_keyframe());
    assert_eq!(pipeline.video_decode_in_flight_packets(), 1);
}

#[test]
fn clear_video_queues_returns_only_queued_resource_handles() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));
    pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(50), 3));
    pipeline.replace_seek_preroll_fallback_video_frame(decoded_frame_for_tests(
        Duration::from_millis(40),
        4,
    ));

    let released_resource_handles = pipeline.clear_video_queues();

    assert_eq!(
        released_resource_handles,
        vec![
            video_core::FrameResourceHandle(1),
            video_core::FrameResourceHandle(2)
        ]
    );
    assert!(pipeline.video_present_queue_is_empty());
    assert_eq!(
        pipeline
            .present_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(3))
    );
    assert_eq!(
        pipeline
            .take_seek_preroll_fallback_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(4))
    );
}
