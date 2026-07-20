use crate::{
    AudioComponentDescriptor, CandidateFormatIdentity, CodecFamily, CodecKind, ContainerIdentity,
    ExtractionGeneration, MuxedComponentDescriptor, NormalizedTransport, RawCodecIdentity,
    RawContainerIdentity, RawTransportIdentity, SourceIdentity, StreamLayoutKind, TransportFamily,
    VideoComponentDescriptor,
};

use super::*;

/// Строит повторяемый video component для layout tests.
fn video_component() -> VideoComponentDescriptor {
    let transport =
        NormalizedTransport::parse(RawTransportIdentity::new("https").expect("transport валиден"));
    let container = ContainerIdentity::parse(
        None,
        Some(RawContainerIdentity::new("webm").expect("container валиден")),
    );
    let codec =
        NormalizedCodec::parse(RawCodecIdentity::new("vp09.00.41.08").expect("codec валиден"));
    let video = VideoTrackDescriptor::new(
        codec,
        Some(VideoWidth::new(1920).expect("width валидна")),
        Some(VideoHeight::new(1080).expect("height валидна")),
        Some(FrameRate::new(30_000, 1001).expect("frame rate валиден")),
        Some(Bitrate::new(4_000_000).expect("video bitrate валиден")),
        DynamicRange::Sdr,
    );

    assert_eq!(
        transport.family(),
        TransportFamily::ProgressiveHttp(crate::HttpScheme::Https)
    );
    assert_eq!(video.codec().kind(), CodecKind::Known(CodecFamily::Vp9));
    VideoComponentDescriptor::new(transport, container, video)
}

/// Строит повторяемый audio component для layout tests.
fn audio_component() -> AudioComponentDescriptor {
    let transport =
        NormalizedTransport::parse(RawTransportIdentity::new("https").expect("transport валиден"));
    let container = ContainerIdentity::parse(
        None,
        Some(RawContainerIdentity::new("webm").expect("container валиден")),
    );
    let codec = NormalizedCodec::parse(RawCodecIdentity::new("opus").expect("codec валиден"));
    let audio = AudioTrackDescriptor::new(
        codec,
        Some(SampleRate::new(48_000).expect("sample rate валидна")),
        Some(ChannelCount::new(2).expect("channels валидны")),
        Some(Bitrate::new(160_000).expect("audio bitrate валиден")),
        None,
    );

    AudioComponentDescriptor::new(transport, container, audio)
}

/// Все четыре layout-а выражаются без Option-инвариантов у caller-а.
#[test]
fn stream_layout_models_muxed_separate_audio_only_and_video_only() {
    let video = video_component();
    let audio = audio_component();
    let muxed = MuxedComponentDescriptor::new(
        video.transport().clone(),
        video.container().clone(),
        video.video().clone(),
        audio.audio().clone(),
    );
    let layouts = [
        StreamLayout::Muxed(muxed),
        StreamLayout::Separate {
            video: video.clone(),
            audio: audio.clone(),
        },
        StreamLayout::AudioOnly(audio),
        StreamLayout::VideoOnly(video),
    ];

    assert_eq!(
        layouts.each_ref().map(|layout| layout.kind()),
        [
            StreamLayoutKind::Muxed,
            StreamLayoutKind::Separate,
            StreamLayoutKind::AudioOnly,
            StreamLayoutKind::VideoOnly,
        ]
    );
    assert_eq!(layouts[2].video_height(), None);
    assert_eq!(
        layouts[3].video_height().map(VideoHeight::pixels),
        Some(1080)
    );
}

/// Candidate сохраняет exact и semantic identities разными полями.
#[test]
fn candidate_keeps_snapshot_and_refresh_identities_separate() {
    let identity = CandidateIdentity::new(
        SourceIdentity::new(1),
        ExtractionGeneration::new(2),
        CandidateFormatIdentity::new("137+140").expect("format id валиден"),
    );
    let semantic = SemanticIdentity::new(SourceIdentity::new(1), "h264-1080p-aac")
        .expect("semantic id валиден");
    let candidate = CandidateDescriptor::new(
        identity.clone(),
        semantic.clone(),
        StreamLayout::VideoOnly(video_component()),
        Vec::new(),
    )
    .expect("candidate валиден");

    assert_eq!(candidate.identity(), &identity);
    assert_eq!(candidate.semantic_identity(), &semantic);
    assert!(candidate.subtitles().is_empty());
}

/// Candidate boundary не разрешает semantic rematch между разными sources.
#[test]
fn candidate_rejects_cross_source_semantic_identity() {
    let identity = CandidateIdentity::new(
        SourceIdentity::new(1),
        ExtractionGeneration::new(2),
        CandidateFormatIdentity::new("137").expect("format id валиден"),
    );
    let semantic =
        SemanticIdentity::new(SourceIdentity::new(2), "h264-1080p").expect("semantic id валиден");

    let error = CandidateDescriptor::new(
        identity,
        semantic,
        StreamLayout::VideoOnly(video_component()),
        Vec::new(),
    )
    .expect_err("cross-source identity должна быть rejected");

    assert_eq!(
        error,
        CandidateDescriptorError::SemanticSourceMismatch {
            exact_source: SourceIdentity::new(1),
            semantic_source: SourceIdentity::new(2),
        }
    );
}

/// Subtitle list не может незаметно смешать identities разных sources.
#[test]
fn candidate_rejects_cross_source_subtitle_identity() {
    let identity = CandidateIdentity::new(
        SourceIdentity::new(1),
        ExtractionGeneration::new(2),
        CandidateFormatIdentity::new("137").expect("format id валиден"),
    );
    let semantic =
        SemanticIdentity::new(SourceIdentity::new(1), "h264-1080p").expect("semantic id валиден");
    let subtitle = SubtitleDescriptor::new(
        SemanticIdentity::new(SourceIdentity::new(2), "subtitle-en")
            .expect("subtitle identity валидна"),
        SubtitleFormatIdentity::new("vtt").expect("subtitle format валиден"),
        Some(LanguageTag::new("en").expect("language валиден")),
    );

    let error = CandidateDescriptor::new(
        identity,
        semantic,
        StreamLayout::VideoOnly(video_component()),
        vec![subtitle],
    )
    .expect_err("cross-source subtitle должна быть rejected");

    assert_eq!(
        error,
        CandidateDescriptorError::SubtitleSourceMismatch {
            ordinal: 0,
            candidate_source: SourceIdentity::new(1),
            subtitle_source: SourceIdentity::new(2),
        }
    );
}

/// Named numeric values не принимают zero и чрезмерную video width.
#[test]
fn descriptor_numeric_values_enforce_named_bounds() {
    assert_eq!(VideoWidth::new(0), Err(VideoWidthError::Zero));
    assert_eq!(
        VideoWidth::new(MAX_VIDEO_WIDTH + 1),
        Err(VideoWidthError::TooLarge {
            provided_pixels: MAX_VIDEO_WIDTH + 1,
            maximum_pixels: MAX_VIDEO_WIDTH,
        })
    );
    assert_eq!(Bitrate::new(0), Err(BitrateError::Zero));
}
