//! Открытие, длительность, ориентация, HDR-метаданные и reset lifecycle.

use super::*;

#[test]
fn non_matroska_audio_extensions_skip_matroska_video_scan() {
    let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

    for extension in ["ogg", "mp3", "wav"] {
        assert_eq!(
            decide_matroska_video_metadata_scan(extension, std::slice::from_ref(&audio_track)),
            MatroskaVideoMetadataScanDecision::SkipNonMatroskaContainer
        );
    }
}

#[test]
fn audio_only_webm_skips_matroska_video_scan_after_symphonia_probe() {
    let audio_track = aac_audio_track_with_timing(2, SymphoniaDuration::new(30_000));

    assert_eq!(
        decide_matroska_video_metadata_scan("webm", &[audio_track]),
        MatroskaVideoMetadataScanDecision::SkipNoVideoCandidates
    );
}

#[test]
fn stream_prefix_scan_limit_stays_bounded() {
    assert_eq!(MATROSKA_STREAM_SCAN_LIMIT_BYTES, 256 * 1024);

    let mut reader = BoundedPrefixReader::new(MATROSKA_STREAM_SCAN_LIMIT_BYTES * 2);
    let (prefix, video_tracks_by_track) =
        read_stream_prefix(&mut reader).expect("bounded prefix read works");

    assert_eq!(prefix.len(), MATROSKA_STREAM_SCAN_LIMIT_BYTES);
    assert_eq!(reader.bytes_read, MATROSKA_STREAM_SCAN_LIMIT_BYTES);
    assert!(video_tracks_by_track.is_empty());
}

#[test]
fn audio_only_demuxer_opens_and_uses_track_duration_metadata() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        Vec::new(),
    );

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "audio-only",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("audio-only demuxer должен открыться без video track-а");

    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Audio);
    assert_eq!(demuxer.tracks()[0].codec_id, "A_AAC");
    assert_eq!(demuxer.tracks()[0].sample_rate, Some(48_000));
    assert_eq!(demuxer.tracks()[0].channels, Some(2));
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

#[test]
fn demuxer_uses_media_info_duration_when_tracks_do_not_have_duration() {
    let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
        .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "media-info-duration",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться с container-level duration");

    assert_eq!(demuxer.tracks().len(), 1);
    assert_eq!(demuxer.tracks()[0].kind, TrackKind::Video);
    assert_eq!(demuxer.tracks()[0].duration, None);
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(12)));
    assert_eq!(demuxer.seekability(), DemuxSeekability::Seekable);
}

#[test]
fn demuxer_maps_per_track_display_orientation_metadata() {
    for orientation in [
        VideoDisplayOrientation::Identity,
        VideoDisplayOrientation::Rotate90Clockwise,
        VideoDisplayOrientation::Rotate180,
        VideoDisplayOrientation::Rotate270Clockwise,
    ] {
        // Metadata приходит в runtime. Black box не позволяет оптимизированному
        // codec-core свернуть const conversion и обойти проверяемый round-trip.
        let reader = FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new())
            .with_display_orientation_metadata(1, std::hint::black_box(orientation));
        let demuxer = SymphoniaDemuxer::from_format_reader(
            Box::new(reader),
            "video-orientation",
            HashMap::new(),
            DemuxSeekability::Seekable,
            DemuxerOptions::default(),
        )
        .expect("fake demuxer должен открыть video track с orientation metadata");
        let video_metadata = demuxer.tracks()[0].video.as_ref();
        if orientation == VideoDisplayOrientation::Identity {
            // Identity без других display overrides не создаёт лишнюю metadata.
            assert!(video_metadata.is_none());
        } else {
            assert_eq!(
                video_metadata
                    .expect("rotation должна создать video metadata")
                    .orientation,
                orientation
            );
        }
    }
}

#[test]
fn demuxer_maps_mp4_per_track_hdr_color_metadata() {
    let reader =
        FakeFormatReader::new(vec![vp9_video_track(1)], Vec::new()).with_mp4_hdr_color_metadata(1);

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "video-color",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыть video track с MP4 color metadata");
    let color = demuxer.tracks()[0]
        .video
        .as_ref()
        .and_then(|metadata| metadata.color.as_ref())
        .expect("MP4 HDR color metadata должна попасть в VideoTrackMetadata.color");

    assert_eq!(color.range, ColorRange::Full);
    assert_eq!(color.matrix, MatrixCoefficients::Bt2020);
    assert_eq!(color.primaries, ColorPrimaries::Bt2020);
    assert_eq!(color.transfer, TransferFunction::Pq);
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_content_light_level_nits),
        Some(1_000)
    );
    assert_eq!(
        color
            .hdr_metadata
            .as_ref()
            .and_then(|metadata| metadata.max_frame_average_light_level_nits),
        Some(400)
    );
}

#[test]
fn demuxer_prefers_track_duration_over_media_info_duration() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        Vec::new(),
    )
    .with_media_info(media_info_with_duration(SymphoniaDuration::new(12_000)));

    let demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "track-duration-wins",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен сохранить track-level duration");

    assert_eq!(demuxer.duration(), Some(Duration::from_secs(30)));
    assert_eq!(demuxer.tracks()[0].duration, Some(Duration::from_secs(30)));
}

#[test]
fn reset_required_refreshes_track_list_as_lifecycle_event() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        vec![Err(SymphoniaError::ResetRequired)],
    )
    .with_reset_track_update(vec![aac_audio_track_with_timing(
        3,
        SymphoniaDuration::new(42_000),
    )]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-event",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let event = demuxer
        .next_event()
        .expect("ResetRequired должен стать lifecycle event");

    match event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks.len(), 1);
            assert_eq!(track_update.tracks[0].id, TrackId::new(3));
            assert_eq!(track_update.duration, Some(Duration::from_secs(42)));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    assert_eq!(demuxer.tracks()[0].id, TrackId::new(3));
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(42)));
}

#[test]
fn reset_required_keeps_media_info_duration_when_tracks_do_not_have_duration() {
    let reader = FakeFormatReader::new(
        vec![vp9_video_track(1)],
        vec![Err(SymphoniaError::ResetRequired)],
    )
    .with_media_info(media_info_with_duration(SymphoniaDuration::new(18_000)))
    .with_reset_track_update(vec![vp9_video_track(4)]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-media-info-duration",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться с container-level duration");

    let event = demuxer
        .next_event()
        .expect("ResetRequired должен обновить track list без потери media duration");

    match event {
        DemuxReadEvent::TracksChanged(track_update) => {
            assert_eq!(track_update.tracks.len(), 1);
            assert_eq!(track_update.tracks[0].id, TrackId::new(4));
            assert_eq!(track_update.duration, Some(Duration::from_secs(18)));
        }
        unexpected_event => panic!("ожидали TracksChanged, получили {unexpected_event:?}"),
    }
    assert_eq!(demuxer.tracks()[0].id, TrackId::new(4));
    assert_eq!(demuxer.tracks()[0].duration, None);
    assert_eq!(demuxer.duration(), Some(Duration::from_secs(18)));
}

#[test]
fn next_event_exposes_reset_lifecycle_before_following_packet() {
    let reader = FakeFormatReader::new(
        vec![aac_audio_track_with_timing(
            2,
            SymphoniaDuration::new(30_000),
        )],
        vec![
            Err(SymphoniaError::ResetRequired),
            Ok(fake_packet(3, 5, b"audio".to_vec())),
        ],
    )
    .with_reset_track_update(vec![aac_audio_track_with_timing(
        3,
        SymphoniaDuration::new(42_000),
    )]);
    let mut demuxer = SymphoniaDemuxer::from_format_reader(
        Box::new(reader),
        "reset-next-packet",
        HashMap::new(),
        DemuxSeekability::Seekable,
        DemuxerOptions::default(),
    )
    .expect("fake demuxer должен открыться");

    let track_event = demuxer
        .next_event()
        .expect("ResetRequired должен стать lifecycle event");
    let packet_event = demuxer
        .next_event()
        .expect("следующий packet нового track-а должен быть доступен");
    let DemuxReadEvent::Packet(packet) = packet_event else {
        panic!("после TracksChanged ожидался packet, получено {packet_event:?}");
    };

    assert!(matches!(track_event, DemuxReadEvent::TracksChanged(_)));
    assert_eq!(packet.track_id, TrackId::new(3));
    assert_eq!(packet.kind, TrackKind::Audio);
}
