use web_media_core::{
    AudioComponentDescriptor, AudioTrackDescriptor, Bitrate, ContainerFamily, ContainerIdentity,
    ContentProbedDescriptor, ContentProbedTrackEvidence, ContentProbedVideoHints, DynamicRange,
    FrameRate, HlsMuxedCodecDeferredBuildError, HlsMuxedCodecDeferredDescriptor,
    MuxedComponentDescriptor, NormalizedCodec, NormalizedTransport, RawCodecIdentity,
    RawContainerIdentity, RawTransportIdentity, StreamLayout, StreamLayoutKind,
    VideoComponentDescriptor, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

fn transport(raw: &str) -> NormalizedTransport {
    NormalizedTransport::parse(
        RawTransportIdentity::new(raw).expect("test transport identity должна быть валидна"),
    )
}

fn container(raw: &str) -> ContainerIdentity {
    ContainerIdentity::parse(
        None,
        Some(RawContainerIdentity::new(raw).expect("test container identity должна быть валидна")),
    )
}

fn codec(raw: &str) -> NormalizedCodec {
    NormalizedCodec::parse(RawCodecIdentity::new(raw).expect("codec identity должна быть валидна"))
}

fn video_track(height: Option<VideoHeight>) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        codec("avc1.640028"),
        Some(VideoWidth::new(1_920).expect("width должна быть ненулевой")),
        height,
        None,
        None,
        DynamicRange::Sdr,
    )
}

fn audio_track() -> AudioTrackDescriptor {
    AudioTrackDescriptor::new(codec("mp4a.40.2"), None, None, None, None)
}

#[test]
fn deferred_hls_layout_preserves_selection_evidence_without_inventing_codecs() {
    let expected_transport = transport("m3u8_native");
    let expected_container = container("mp4");
    let expected_height = VideoHeight::new(1_080).expect("height должна быть ненулевой");
    let expected_width = VideoWidth::new(1_920).expect("width должна быть ненулевой");
    let expected_frame_rate =
        FrameRate::new(60_000, 1_001).expect("frame rate должна быть валидна");
    let expected_bitrate = Bitrate::new(8_000_000).expect("bitrate должен быть ненулевым");

    let descriptor = HlsMuxedCodecDeferredDescriptor::new(
        expected_transport.clone(),
        expected_container.clone(),
        expected_height,
        Some(expected_width),
        Some(expected_frame_rate),
        Some(expected_bitrate),
        DynamicRange::Hdr,
    )
    .expect("HLS transport должен допускать отложенное codec proof");

    assert_eq!(descriptor.transport(), &expected_transport);
    assert_eq!(descriptor.container(), &expected_container);
    assert_eq!(descriptor.height(), expected_height);
    assert_eq!(descriptor.width(), Some(expected_width));
    assert_eq!(descriptor.frame_rate(), Some(expected_frame_rate));
    assert_eq!(descriptor.bitrate(), Some(expected_bitrate));
    assert_eq!(descriptor.dynamic_range(), DynamicRange::Hdr);

    let layout = StreamLayout::HlsMuxedCodecDeferred(descriptor);
    assert_eq!(layout.kind(), StreamLayoutKind::HlsMuxedCodecDeferred);
    assert_eq!(layout.video_height(), Some(expected_height));
    assert_eq!(layout.video_height_hint(), Some(expected_height));
}

#[test]
fn deferred_hls_layout_rejects_non_hls_transport_before_catalog_publication() {
    let result = HlsMuxedCodecDeferredDescriptor::new(
        transport("https"),
        container("mp4"),
        VideoHeight::new(720).expect("height должна быть ненулевой"),
        None,
        None,
        None,
        DynamicRange::Sdr,
    );

    assert_eq!(
        result,
        Err(HlsMuxedCodecDeferredBuildError::TransportNotHls),
        "обычный HTTP resource нельзя выдать за HLS ladder без codec evidence"
    );
}

#[test]
fn every_layout_shape_keeps_proven_height_separate_from_soft_probe_hints() {
    let http_transport = transport("https");
    let mp4_container = container("mp4");
    let proven_height = VideoHeight::new(1_080).expect("height должна быть ненулевой");
    let soft_height = VideoHeight::new(720).expect("height должна быть ненулевой");
    let video = VideoComponentDescriptor::new(
        http_transport.clone(),
        mp4_container.clone(),
        video_track(Some(proven_height)),
    );
    let audio =
        AudioComponentDescriptor::new(http_transport.clone(), mp4_container.clone(), audio_track());
    assert_eq!(audio.transport(), &http_transport);
    assert_eq!(audio.container(), &mp4_container);
    assert_eq!(audio.audio().codec(), &codec("mp4a.40.2"));

    let muxed = MuxedComponentDescriptor::new(
        http_transport.clone(),
        mp4_container.clone(),
        video_track(Some(proven_height)),
        audio_track(),
    );
    assert_eq!(muxed.transport(), &http_transport);
    assert_eq!(muxed.container(), &mp4_container);
    assert_eq!(muxed.video().height(), Some(proven_height));
    assert_eq!(muxed.audio().codec(), &codec("mp4a.40.2"));

    let video_hints =
        ContentProbedVideoHints::new(None, Some(soft_height), None, None, DynamicRange::Sdr);
    assert_eq!(video_hints.width(), None);
    assert_eq!(video_hints.height(), Some(soft_height));
    assert_eq!(video_hints.frame_rate(), None);
    assert_eq!(video_hints.bitrate(), None);
    assert_eq!(video_hints.dynamic_range(), DynamicRange::Sdr);

    let probed_unknown_video = ContentProbedDescriptor::new(
        http_transport.clone(),
        container("ogg"),
        ContainerFamily::Ogg,
        ContentProbedTrackEvidence::Unknown,
        ContentProbedTrackEvidence::Declared(audio_track()),
        video_hints,
    )
    .expect("audio proof должен допускать unknown video topology");
    assert_eq!(probed_unknown_video.transport(), &http_transport);
    assert_eq!(probed_unknown_video.container(), &container("ogg"));
    assert_eq!(probed_unknown_video.probe_container(), ContainerFamily::Ogg);
    assert!(matches!(
        probed_unknown_video.video(),
        ContentProbedTrackEvidence::Unknown
    ));
    assert!(matches!(
        probed_unknown_video.audio(),
        ContentProbedTrackEvidence::Declared(_)
    ));
    assert_eq!(
        probed_unknown_video.video_hints().height(),
        Some(soft_height)
    );

    let cases = [
        (
            StreamLayout::Muxed(muxed),
            StreamLayoutKind::Muxed,
            Some(proven_height),
            Some(proven_height),
        ),
        (
            StreamLayout::Separate {
                video: video.clone(),
                audio: audio.clone(),
            },
            StreamLayoutKind::Separate,
            Some(proven_height),
            Some(proven_height),
        ),
        (
            StreamLayout::VideoOnly(video),
            StreamLayoutKind::VideoOnly,
            Some(proven_height),
            Some(proven_height),
        ),
        (
            StreamLayout::AudioOnly(audio),
            StreamLayoutKind::AudioOnly,
            None,
            None,
        ),
        (
            StreamLayout::ContentProbed(probed_unknown_video),
            StreamLayoutKind::ContentProbed,
            None,
            Some(soft_height),
        ),
    ];

    for (layout, expected_kind, expected_height, expected_hint) in cases {
        assert_eq!(layout.kind(), expected_kind);
        assert_eq!(layout.video_height(), expected_height);
        assert_eq!(layout.video_height_hint(), expected_hint);
    }
}
