use super::*;

fn composite_from_scripted_components(
    video_demuxer: ScriptedDemuxer,
    audio_demuxer: ScriptedDemuxer,
) -> CompositeAvDemuxer {
    let selection =
        CompositeAvTrackSelection::new(video_demuxer.tracks[0].id, audio_demuxer.tracks[0].id);
    CompositeAvDemuxer::new(
        Box::new(video_demuxer),
        Box::new(audio_demuxer),
        selection,
        lead_policy(),
    )
    .expect("scripted composite opens")
}

/// Ошибка чтения сохраняет component ownership и concrete backend-причину.
#[test]
fn component_read_failures_preserve_video_and_audio_ownership() {
    let cases = [
        (CompositeComponent::Video, "video-read-failure"),
        (CompositeComponent::Audio, "audio-read-failure"),
    ];

    for (failing_component, expected_reason) in cases {
        let video_events = if failing_component == CompositeComponent::Audio {
            VecDeque::from([DemuxReadEvent::Packet(packet(
                11,
                TrackKind::Video,
                0,
                b"video-before-audio-error",
            ))])
        } else {
            VecDeque::new()
        };
        let mut video_demuxer = ScriptedDemuxer::new(
            vec![track(11, TrackKind::Video, "V_H264", 10_000)],
            Some(Duration::from_secs(10)),
            video_events,
            SeekBehavior::Success(MediaTime::ZERO),
            Arc::new(Mutex::new(Vec::new())),
        );
        let mut audio_demuxer = ScriptedDemuxer::new(
            vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
            Some(Duration::from_secs(10)),
            VecDeque::new(),
            SeekBehavior::Success(MediaTime::ZERO),
            Arc::new(Mutex::new(Vec::new())),
        );
        match failing_component {
            CompositeComponent::Video => {
                video_demuxer = video_demuxer.with_next_event_error(expected_reason);
            }
            CompositeComponent::Audio => {
                audio_demuxer = audio_demuxer.with_next_event_error(expected_reason);
            }
        }
        let mut composite = composite_from_scripted_components(video_demuxer, audio_demuxer);

        let error = composite
            .next_event()
            .expect_err("component read failure must stop composite publication");
        let typed_error = error
            .downcast_ref::<CompositeComponentReadError>()
            .expect("component read failure keeps typed ownership context");
        let source_error = typed_error
            .source
            .downcast_ref::<ScriptedReadError>()
            .expect("concrete component source remains downcastable");
        assert_eq!(typed_error.component, failing_component);
        assert_eq!(source_error.safe_reason, expected_reason);
    }
}

/// Metadata lifecycle меняет snapshot до event и сохраняет packet-to-EOF progression.
#[test]
fn component_metadata_change_refreshes_merged_snapshot_before_event() {
    let updated_video_metadata = MediaMetadata {
        tags: MediaTagMetadata {
            title: Some("Updated video title".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let audio_metadata = MediaMetadata {
        tags: MediaTagMetadata {
            artists: vec!["Audio artist".into()],
            album: Some("Audio album".into()),
            ..Default::default()
        },
        ..Default::default()
    };
    let video_demuxer = ScriptedDemuxer::new(
        vec![track(11, TrackKind::Video, "V_H264", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::from([
            DemuxReadEvent::MediaMetadataChanged(updated_video_metadata),
            DemuxReadEvent::Packet(packet(11, TrackKind::Video, 4_000, b"video")),
        ]),
        SeekBehavior::Success(MediaTime::ZERO),
        Arc::new(Mutex::new(Vec::new())),
    );
    let audio_demuxer = ScriptedDemuxer::new(
        vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::from([DemuxReadEvent::Packet(packet(
            22,
            TrackKind::Audio,
            5_000,
            b"audio",
        ))]),
        SeekBehavior::Success(MediaTime::ZERO),
        Arc::new(Mutex::new(Vec::new())),
    )
    .with_media_metadata(audio_metadata);
    let mut composite = composite_from_scripted_components(video_demuxer, audio_demuxer);

    let initial_metadata = composite
        .media_metadata()
        .expect("audio fallback is visible before video update");
    assert!(initial_metadata.tags.title.is_none());
    assert_eq!(initial_metadata.tags.artists, ["Audio artist"]);

    let published_metadata = match composite
        .next_event()
        .expect("metadata change must be published before media")
    {
        DemuxReadEvent::MediaMetadataChanged(media_metadata) => media_metadata,
        other => panic!("expected metadata change, got {other:?}"),
    };
    assert_eq!(
        published_metadata.tags.title.as_deref(),
        Some("Updated video title")
    );
    assert_eq!(published_metadata.tags.artists, ["Audio artist"]);
    assert_eq!(
        published_metadata.tags.album.as_deref(),
        Some("Audio album")
    );
    assert_eq!(composite.media_metadata(), Some(published_metadata));

    let first_packet = composite
        .next_event()
        .expect("metadata refresh preserves first packet");
    let second_packet = composite
        .next_event()
        .expect("metadata refresh preserves second packet");
    assert!(matches!(
        first_packet,
        DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Video
    ));
    assert!(matches!(
        second_packet,
        DemuxReadEvent::Packet(packet) if packet.kind == TrackKind::Audio
    ));
    assert_eq!(
        composite
            .next_event()
            .expect("metadata refresh must preserve terminal lifecycle"),
        DemuxReadEvent::EndOfStream
    );
}

/// Decode-point seek разрешает ровно один audio bootstrap до раннего video preroll.
#[test]
fn decode_point_seek_bootstraps_audio_once_then_restores_normal_interleave() {
    let video_demuxer = ScriptedDemuxer::new(
        vec![track(11, TrackKind::Video, "V_H264", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::from([DemuxReadEvent::Packet(packet(
            11,
            TrackKind::Video,
            4_000,
            b"video-preroll",
        ))]),
        SeekBehavior::Success(MediaTime::from_duration(Duration::from_secs(4))),
        Arc::new(Mutex::new(Vec::new())),
    );
    let audio_demuxer = ScriptedDemuxer::new(
        vec![track(22, TrackKind::Audio, "A_AAC", 10_000)],
        Some(Duration::from_secs(10)),
        VecDeque::from([
            DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 5_000, b"audio-bootstrap")),
            DemuxReadEvent::Packet(packet(22, TrackKind::Audio, 6_000, b"audio-after-preroll")),
        ]),
        SeekBehavior::Success(MediaTime::from_duration(Duration::from_secs(5))),
        Arc::new(Mutex::new(Vec::new())),
    );
    let mut composite = composite_from_scripted_components(video_demuxer, audio_demuxer);
    composite
        .seek_with_request(DemuxSeekRequest::decode_point_before(Duration::from_secs(
            5,
        )))
        .expect("decode-point seek arms the one-shot audio bootstrap");

    let first = composite.next_event().expect("bootstrap audio packet");
    let second = composite.next_event().expect("video preroll packet");
    let third = composite.next_event().expect("remaining audio packet");
    assert!(matches!(
        first,
        DemuxReadEvent::Packet(packet)
            if packet.kind == TrackKind::Audio && packet.data.as_ref() == b"audio-bootstrap"
    ));
    assert!(matches!(
        second,
        DemuxReadEvent::Packet(packet)
            if packet.kind == TrackKind::Video && packet.data.as_ref() == b"video-preroll"
    ));
    assert!(matches!(
        third,
        DemuxReadEvent::Packet(packet)
            if packet.kind == TrackKind::Audio && packet.data.as_ref() == b"audio-after-preroll"
    ));
    assert_eq!(
        composite.next_event().expect("post-seek streams reach EOF"),
        DemuxReadEvent::EndOfStream
    );
}
