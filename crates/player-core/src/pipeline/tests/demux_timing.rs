//! Границы demux, выбранные треки, тайминг кадров и FIFO pending-пакетов.

use super::*;

#[test]
fn demux_boundaries_preserve_eof_and_seek_results() {
    let mut pipeline = PlaybackPipeline::default();
    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(Vec::new())),
        None,
        None,
        Vec::new(),
    );

    let event = pipeline
        .demux_next_event()
        .expect("installed demuxer должен быть видим через boundary")
        .expect("fake demuxer не должен возвращать ошибку");
    assert_eq!(event, DemuxReadEvent::EndOfStream);

    let seek_result = pipeline
        .seek_demuxer(DemuxSeekRequest::accurate(Duration::from_secs(3)))
        .expect("installed demuxer должен принять seek через boundary")
        .expect("fake demuxer должен принять accurate seek");
    assert_eq!(seek_result.actual_position, MediaTime::from_secs(3));
}

#[test]
fn selected_track_boundaries_manage_ids_requirement_and_clear_only_selection() {
    let mut pipeline = PlaybackPipeline::default();
    let video_track_id = TrackId::new(10);
    let audio_track_id = TrackId::new(20);
    let source_tracks = vec![
        source_slot_track(video_track_id, TrackKind::Video, "V_VP9"),
        source_slot_track(audio_track_id, TrackKind::Audio, "A_OPUS"),
    ];
    let initial_requirement = VideoDecodeRequirement::new(VideoCodec::Vp9);
    let refined_requirement = initial_requirement.clone().with_resolution(1920, 1080);

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(source_tracks.clone())),
        None,
        Some("selected-track-test".to_owned()),
        source_tracks,
    );
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        audio_track_id,
        Duration::from_millis(1),
        None,
        None,
        pipeline.seek_generation(),
        Bytes::from_static(b"audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(2),
        pipeline.seek_generation(),
        Bytes::from_static(b"video"),
        true,
    ));

    pipeline.select_audio_track(audio_track_id);
    pipeline.select_video_track(video_track_id, initial_requirement.clone());

    assert_eq!(pipeline.selected_audio_track_id(), Some(audio_track_id));
    assert_eq!(pipeline.selected_video_track_id(), Some(video_track_id));
    assert!(pipeline.has_selected_audio_track());
    assert!(pipeline.has_selected_video_track());
    assert!(pipeline.video_packet_belongs_to_selected_track(video_track_id));
    assert!(!pipeline.video_packet_belongs_to_selected_track(TrackId::new(99)));
    assert_eq!(
        pipeline.active_video_requirement(),
        Some(&initial_requirement)
    );

    pipeline.set_active_video_requirement(refined_requirement.clone());

    assert_eq!(
        pipeline.active_video_requirement(),
        Some(&refined_requirement)
    );

    pipeline.clear_selected_tracks();

    assert!(pipeline.selected_audio_track_id().is_none());
    assert!(pipeline.selected_video_track_id().is_none());
    assert!(!pipeline.has_selected_audio_track());
    assert!(!pipeline.has_selected_video_track());
    assert!(pipeline.active_video_requirement().is_none());
    assert_eq!(pipeline.pending_audio_packet_len(), 1);
    assert_eq!(pipeline.pending_video_packet_len(), 1);
    assert!(pipeline.has_demuxer());
    assert_eq!(pipeline.track_count(), 2);
}

#[test]
fn video_frame_timing_first_observation_only_records_pts() {
    let mut pipeline = PlaybackPipeline::default();

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn video_frame_timing_valid_delta_updates_estimate_with_legacy_smoothing() {
    let mut pipeline = PlaybackPipeline::default();
    let observed_frame_duration = Duration::from_millis(20);

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);

    let old_micros = DEFAULT_VIDEO_FRAME_DURATION.as_micros() as u64;
    let observed_micros = observed_frame_duration.as_micros() as u64;
    let expected_micros = (old_micros.saturating_mul(7) + observed_micros) / 8;

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        Duration::from_micros(expected_micros.max(1))
    );
}

#[test]
fn video_frame_timing_ignores_out_of_range_deltas() {
    let mut pipeline = PlaybackPipeline::default();
    let first_pts = Duration::from_secs(10);
    let too_small_pts = first_pts + MIN_OBSERVED_VIDEO_FRAME_DURATION / 2;
    let too_large_pts = too_small_pts + MAX_OBSERVED_VIDEO_FRAME_DURATION * 2;

    pipeline.observe_decoded_video_frame_pts(first_pts);
    pipeline.observe_decoded_video_frame_pts(too_small_pts);
    pipeline.observe_decoded_video_frame_pts(too_large_pts);

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn video_frame_timing_reset_restores_default_and_clears_previous_pts() {
    let mut pipeline = PlaybackPipeline::default();
    let observed_frame_duration = Duration::from_millis(20);

    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10));
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration);
    assert_ne!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );

    pipeline.reset_video_frame_timing_estimator();
    pipeline.observe_decoded_video_frame_pts(Duration::from_secs(10) + observed_frame_duration * 2);

    assert_eq!(
        pipeline.video_frame_duration_estimate(),
        DEFAULT_VIDEO_FRAME_DURATION
    );
}

#[test]
fn pending_packet_queue_boundaries_preserve_fifo_order_and_lengths() {
    let mut pipeline = PlaybackPipeline::default();
    let audio_track_id = TrackId::new(20);
    let video_track_id = TrackId::new(10);
    let generation = pipeline.seek_generation();

    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());

    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        audio_track_id,
        Duration::from_millis(10),
        None,
        None,
        generation,
        Bytes::from_static(b"audio-10"),
    ));
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        audio_track_id,
        Duration::from_millis(20),
        None,
        None,
        generation,
        Bytes::from_static(b"audio-20"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(30),
        generation,
        Bytes::from_static(b"video-30"),
        true,
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        video_track_id,
        Duration::from_millis(40),
        generation,
        Bytes::from_static(b"video-40"),
        false,
    ));

    assert_eq!(pipeline.pending_audio_packet_len(), 2);
    assert_eq!(pipeline.pending_video_packet_len(), 2);
    assert_eq!(
        pipeline
            .front_pending_video_packet()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(30))
    );
    assert_eq!(
        pipeline
            .pop_pending_audio_packet_front()
            .map(|packet| packet.pts()),
        Some(Duration::from_millis(10))
    );
    assert_eq!(
        pipeline
            .pop_pending_audio_packet_front()
            .map(|packet| packet.pts()),
        Some(Duration::from_millis(20))
    );
    assert_eq!(
        pipeline
            .pop_pending_video_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(30))
    );
    assert_eq!(
        pipeline
            .pop_pending_video_packet_front()
            .map(|packet| packet.pts),
        Some(Duration::from_millis(40))
    );
    assert!(pipeline.pending_audio_packet_is_empty());
    assert!(pipeline.pending_video_packet_is_empty());
}
