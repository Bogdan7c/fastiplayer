//! Очереди кадров, present ownership и слоты установленного источника.

use super::*;

#[test]
fn queued_video_frame_methods_preserve_fifo_order_and_len() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(pipeline.video_present_queue_is_empty());
    assert_eq!(pipeline.video_present_queue_len(), 0);

    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(16), 1));
    pipeline.enqueue_queued_video_frame(decoded_frame_for_tests(Duration::from_millis(33), 2));

    assert_eq!(pipeline.video_present_queue_len(), 2);
    assert_eq!(
        pipeline.front_queued_video_frame().map(|frame| frame.pts),
        Some(Duration::from_millis(16))
    );
    assert_eq!(
        pipeline
            .front_and_next_queued_video_frames()
            .map(|(front_frame, next_frame)| (front_frame.pts, next_frame.pts)),
        Some((Duration::from_millis(16), Duration::from_millis(33)))
    );

    assert_eq!(
        pipeline
            .pop_queued_video_frame_front()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(1))
    );
    assert_eq!(
        pipeline
            .pop_queued_video_frame_front()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(2))
    );
    assert!(pipeline.pop_queued_video_frame_front().is_none());
    assert!(pipeline.video_present_queue_is_empty());
}

#[test]
fn queued_video_frame_covers_target_for_generation_reports_only_matching_frames() {
    let mut pipeline = PlaybackPipeline::default();
    let target_position = Duration::from_millis(100);
    let active_generation = 7;
    let stale_generation = 6;

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut pretarget_frame = decoded_frame_for_tests(Duration::from_millis(90), 1);
    pretarget_frame.generation = active_generation;
    pipeline.enqueue_queued_video_frame(pretarget_frame);

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut stale_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 2);
    stale_target_frame.generation = stale_generation;
    pipeline.enqueue_queued_video_frame(stale_target_frame);

    assert!(
        !pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );

    let mut active_target_frame = decoded_frame_for_tests(Duration::from_millis(100), 3);
    active_target_frame.generation = active_generation;
    pipeline.enqueue_queued_video_frame(active_target_frame);

    assert!(
        pipeline
            .queued_video_frame_covers_target_for_generation(target_position, active_generation)
    );
}

#[test]
fn present_video_frame_methods_keep_replacement_ownership_explicit() {
    let mut pipeline = PlaybackPipeline::default();

    assert!(!pipeline.has_present_video_frame());
    assert!(pipeline.present_video_frame().is_none());
    assert!(pipeline.take_present_video_frame().is_none());

    pipeline.set_present_video_frame(decoded_frame_for_tests(Duration::from_millis(10), 10));

    assert!(pipeline.has_present_video_frame());
    assert_eq!(
        pipeline.present_video_frame().map(|frame| frame.pts),
        Some(Duration::from_millis(10))
    );

    let replaced_frame = pipeline
        .replace_present_video_frame(decoded_frame_for_tests(Duration::from_millis(20), 20));

    assert_eq!(
        replaced_frame.map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(10))
    );
    assert_eq!(
        pipeline
            .take_present_video_frame()
            .map(|frame| frame.resource_handle),
        Some(video_core::FrameResourceHandle(20))
    );
    assert!(!pipeline.has_present_video_frame());
}

#[test]
fn opened_media_boundary_methods_expose_installed_source_slots() {
    let mut pipeline = PlaybackPipeline::default();
    let track_infos = vec![
        source_slot_track(TrackId::new(1), TrackKind::Video, "V_VP9"),
        source_slot_track(TrackId::new(2), TrackKind::Audio, "A_OPUS"),
    ];

    assert!(!pipeline.has_demuxer());
    assert_eq!(pipeline.track_count(), 0);

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(track_infos.clone())),
        Some(PathBuf::from("/tmp/source.webm")),
        Some("external source".to_owned()),
        track_infos,
    );

    assert!(pipeline.has_demuxer());
    assert_eq!(
        pipeline.source_file_path(),
        Some(Path::new("/tmp/source.webm"))
    );
    assert_eq!(pipeline.source_label(), Some("external source"));
    assert_eq!(pipeline.track_count(), 2);
    assert_eq!(pipeline.tracks()[0].id, TrackId::new(1));
    assert_eq!(pipeline.tracks()[1].kind, TrackKind::Audio);
}

#[test]
fn demux_track_list_update_invalidates_decoder_dependent_state() {
    let mut pipeline = PlaybackPipeline::default();
    let old_video_track = TrackId::new(1);
    let old_audio_track = TrackId::new(2);
    let new_audio_track = TrackId::new(3);
    let initial_tracks = vec![
        source_slot_track(old_video_track, TrackKind::Video, "V_VP9"),
        source_slot_track(old_audio_track, TrackKind::Audio, "A_OPUS"),
    ];

    pipeline.install_opened_media(
        Box::new(SourceSlotFakeDemuxer::new(initial_tracks.clone())),
        None,
        None,
        initial_tracks,
    );
    pipeline.select_video_track(
        old_video_track,
        VideoDecodeRequirement::new(VideoCodec::Vp9),
    );
    pipeline.select_audio_track(old_audio_track);
    pipeline.install_deferred_audio_decoder_config(
        audio_core::AudioDecoderConfig::from_track_metadata(
            old_audio_track.get(),
            "A_OPUS",
            Some(48_000),
            Some(2),
        ),
    );
    pipeline.install_audio_decoder(Box::new(FakeAudioDecoder::with_samples(
        vec![0.0],
        48_000,
        2,
    )));
    pipeline.enqueue_pending_audio_packet(PendingAudioPacket::new_unbounded(
        old_audio_track,
        Duration::ZERO,
        None,
        None,
        pipeline.seek_generation(),
        Bytes::from_static(b"old audio"),
    ));
    pipeline.enqueue_pending_video_packet(PendingVideoPacket::new(
        old_video_track,
        Duration::ZERO,
        pipeline.seek_generation(),
        Bytes::from_static(b"old video"),
        true,
    ));
    pipeline.mark_video_decoder_bootstrapped();
    pipeline.note_video_packet_sent_to_decoder();

    pipeline.apply_demux_track_list_update(vec![source_slot_track(
        new_audio_track,
        TrackKind::Audio,
        "A_AAC",
    )]);

    assert_eq!(pipeline.tracks()[0].id, new_audio_track);
    assert!(pipeline.selected_audio_track_id().is_none());
    assert!(pipeline.selected_video_track_id().is_none());
    assert!(!pipeline.has_audio_decoder());
    assert!(!pipeline.has_deferred_audio_decoder_config());
    assert_eq!(pipeline.pending_audio_packet_len(), 0);
    assert_eq!(pipeline.pending_video_packet_len(), 0);
    assert!(!pipeline.video_decoder_needs_keyframe());
    assert_eq!(pipeline.video_decode_in_flight_packets(), 0);
}
