use bounded_xml_reader::{XmlBudgets, XmlReadError};
use smooth_streaming_manifest_core::{
    SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND, SmoothCodecConfigurationError,
    SmoothCodecConfigurationOrigin, SmoothDeclaredCountKind, SmoothManifest, SmoothManifestError,
    SmoothManifestLimitKind, SmoothManifestLimits, SmoothManifestParseRequest,
    SmoothManifestVersion, SmoothProfileIncompatibility, SmoothQualityLevel, SmoothStreamKind,
    SmoothTimelineError, SmoothUnsupportedConstruct, parse_vod_client_manifest,
    parse_vod_client_manifest_cancellable,
};

const VALID_V20: &str = include_str!("fixtures/valid_h264_aac_v20.ismc");
const VALID_V22_REPEAT: &str = include_str!("fixtures/valid_repeat_v22.ismc");
const TIMELINE_INFERENCE: &str = include_str!("fixtures/timeline_inference.ismc");
const DIFFERING_AV_TIMESCALES: &str =
    include_str!("fixtures/differing_av_timescales_alignment.ismc");
const DRM_PLAYREADY: &str = include_str!("fixtures/drm_playready.ismc");
const MALFORMED_XML: &str = include_str!("fixtures/malformed_xml.ismc");
const EXTERNAL_ENTITY: &str = include_str!("fixtures/external_entity.ismc");
const NEGATIVE_REPEAT: &str = include_str!("fixtures/negative_repeat.ismc");
const UNSUPPORTED_CODEC: &str = include_str!("fixtures/unsupported_codec.ismc");
const MALFORMED_CODEC_PRIVATE_DATA: &str =
    include_str!("fixtures/malformed_codec_private_data.ismc");

/// Integration tests задают оба budget набора явно, как обязан production caller.
fn parse(document: &str) -> Result<SmoothManifest, SmoothManifestError> {
    parse_vod_client_manifest(request(document))
}

/// Создаёт caller-owned request без скрытых defaults в production API.
fn request(document: &str) -> SmoothManifestParseRequest<'_> {
    SmoothManifestParseRequest {
        document_bytes: document.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: manifest_limits(),
    }
}

/// Test-only hardened XML budgets.
fn xml_budgets() -> XmlBudgets {
    XmlBudgets::builder()
        .maximum_document_bytes(64 * 1024)
        .maximum_depth(16)
        .maximum_tokens(2_048)
        .maximum_attributes_per_element(32)
        .maximum_attribute_count(1_024)
        .maximum_attribute_bytes(48 * 1024)
        .maximum_namespace_declarations_per_element(8)
        .maximum_namespace_declaration_count(32)
        .maximum_namespace_bytes(4 * 1024)
        .maximum_text_bytes(32 * 1024)
        .build()
        .expect("test XML budgets полны")
}

/// Test-only schema limits соответствуют малому VOD manifest-у.
fn manifest_limits() -> SmoothManifestLimits {
    manifest_limits_with_total_custom_attributes(32)
}

/// Позволяет focused test исчерпать только manifest-wide CustomAttributes budget.
fn manifest_limits_with_total_custom_attributes(maximum: usize) -> SmoothManifestLimits {
    SmoothManifestLimits::builder()
        .maximum_streams(8)
        .maximum_qualities_per_stream(16)
        .maximum_total_qualities(32)
        .maximum_timeline_entries_per_stream(128)
        .maximum_total_timeline_entries(256)
        .maximum_fragments_per_stream(1_024)
        .maximum_total_fragments(2_048)
        .maximum_template_bytes(512)
        .maximum_string_bytes(256)
        .maximum_codec_bytes(4_096)
        .maximum_custom_attributes_per_quality(maximum.min(8))
        .maximum_total_custom_attributes(maximum)
        .maximum_custom_attribute_name_bytes(64)
        .maximum_custom_attribute_value_bytes(128)
        .build()
        .expect("test schema limits полны")
}

#[test]
fn explicit_h264_aac_fixture_preserves_shape_clocks_and_codec_proofs() {
    let manifest = parse(VALID_V20).expect("valid v2.0 fixture принимается");
    assert_eq!(manifest.version(), SmoothManifestVersion::V2_0);
    assert_eq!(manifest.duration().ticks(), 20_000_000);
    assert_eq!(manifest.streams().len(), 2);

    let video = &manifest.streams()[0];
    assert_eq!(video.kind(), SmoothStreamKind::Video);
    assert_eq!(video.timescale().get(), 10_000_000);
    assert_eq!(video.timeline().fragment_count(), 2);
    let SmoothQualityLevel::Video(video_quality) = &video.qualities()[0] else {
        panic!("первый stream обязан остаться video");
    };
    assert_eq!(video_quality.codec().as_str(), "H264");
    assert_eq!(
        video_quality.codec_configuration().origin(),
        SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets
    );
    assert_eq!(video_quality.custom_attributes().len(), 1);

    let audio = &manifest.streams()[1];
    assert_eq!(audio.kind(), SmoothStreamKind::Audio);
    assert_eq!(audio.timescale().get(), 48_000);
    assert_eq!(audio.timeline().last_end().ticks(), 96_000);
    let SmoothQualityLevel::Audio(audio_quality) = &audio.qualities()[0] else {
        panic!("второй stream обязан остаться audio");
    };
    assert_eq!(
        audio_quality.codec_configuration().origin(),
        SmoothCodecConfigurationOrigin::AacAudioSpecificConfig
    );
}

#[test]
fn version_22_repeat_is_total_two_and_stream_inherits_root_timescale() {
    let manifest = parse(VALID_V22_REPEAT).expect("valid v2.2 repeat fixture принимается");
    let stream = &manifest.streams()[0];
    assert_eq!(stream.timescale().get(), 10);
    assert_eq!(stream.timeline().fragment_count(), 2);
    assert_eq!(stream.timeline().fragment_at(0).unwrap().start().ticks(), 0);
    assert_eq!(
        stream.timeline().fragment_at(1).unwrap().start().ticks(),
        10
    );
}

#[test]
fn default_root_clock_and_cross_timescale_alignment_are_exact() {
    let manifest = parse(DIFFERING_AV_TIMESCALES).expect("aligned A/V fixture валиден");
    assert_eq!(
        manifest.duration().timescale().get(),
        SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND
    );
    assert_eq!(manifest.streams()[0].timescale().get(), 10_000_000);
    assert_eq!(manifest.streams()[1].timescale().get(), 48_000);
    assert_eq!(manifest.streams()[0].name().unwrap().as_str(), "video-main");
    assert_eq!(manifest.streams()[1].language().unwrap().as_str(), "uk-UA");
    assert_eq!(manifest.streams()[0].qualities()[0].index().get(), 10);
    assert_eq!(manifest.streams()[1].qualities()[0].index().get(), 20);

    let inline_document = valid_video_document("", "").replace(r#" TimeScale="10""#, "");
    let inline_without_root_timescale =
        parse(&inline_document).expect("inline default clock fixture валиден");
    assert_eq!(
        inline_without_root_timescale.duration().timescale().get(),
        SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND
    );

    let zero_clock = DIFFERING_AV_TIMESCALES.replacen(
        r#"Duration="20000000""#,
        r#"TimeScale="0" Duration="20000000""#,
        1,
    );
    assert_eq!(
        parse(&zero_clock),
        Err(SmoothManifestError::MalformedSchema {
            field: smooth_streaming_manifest_core::SmoothSchemaField::TimeScale,
        })
    );
}

#[test]
fn required_stream_attributes_and_quality_identity_fail_closed() {
    for (removed, field) in [
        (
            r#" Chunks="2""#,
            smooth_streaming_manifest_core::SmoothSchemaField::StreamIndex,
        ),
        (
            r#" QualityLevels="1""#,
            smooth_streaming_manifest_core::SmoothSchemaField::StreamIndex,
        ),
        (
            r#" Url="QualityLevels({bitrate})/Fragments(video={start_time})""#,
            smooth_streaming_manifest_core::SmoothSchemaField::Url,
        ),
    ] {
        assert_eq!(
            parse(&VALID_V22_REPEAT.replacen(removed, "", 1)),
            Err(SmoothManifestError::MalformedSchema { field })
        );
    }

    let duplicate_index =
        two_quality_document("q({bitrate})/v({start time})", 0, 2_000, None, None);
    assert_eq!(
        parse(&duplicate_index),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::DuplicateQualityIndex,
        })
    );

    let ambiguous_bitrate =
        two_quality_document("q({bitrate})/v({start time})", 1, 1_000, None, None);
    assert_eq!(
        parse(&ambiguous_bitrate),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::AmbiguousQualityRenderIdentity,
        })
    );
}

#[test]
fn custom_attributes_disambiguate_bitrate_and_obey_manifest_total_budget() {
    let distinct_attributes = two_quality_document(
        "q({bitrate},{CustomAttributes})/v({start time})",
        1,
        1_000,
        Some(("role", "main")),
        Some(("role", "alternate")),
    );
    parse(&distinct_attributes).expect("разные typed attributes disambiguate одинаковый bitrate");

    let identical_attributes = two_quality_document(
        "q({bitrate},{CustomAttributes})/v({start time})",
        1,
        1_000,
        Some(("role", "main")),
        Some(("role", "main")),
    );
    assert_eq!(
        parse(&identical_attributes),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::AmbiguousQualityRenderIdentity,
        })
    );

    let error = parse_vod_client_manifest(SmoothManifestParseRequest {
        document_bytes: distinct_attributes.as_bytes(),
        xml_budgets: xml_budgets(),
        limits: manifest_limits_with_total_custom_attributes(1),
    })
    .expect_err("manifest-wide attributes budget обязан примениться до publication");
    assert_eq!(
        error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TotalCustomAttributes,
            maximum: 1,
        }
    );
}

#[test]
fn presentation_duration_and_common_interval_use_exact_cross_products() {
    let outside_duration = VALID_V22_REPEAT.replace(r#"Duration="20""#, r#"Duration="19""#);
    assert_eq!(
        parse(&outside_duration),
        Err(SmoothManifestError::InvalidTimeline {
            reason: SmoothTimelineError::OutsidePresentationDuration,
        })
    );

    let zero_overlap = r#"
<SmoothStreamingMedia MajorVersion="2" MinorVersion="0" Duration="20000000">
  <StreamIndex Type="video" Chunks="1" QualityLevels="1" Url="q({bitrate})/v({start time})">
    <QualityLevel Index="0" Bitrate="1000" FourCC="H264" MaxWidth="16" MaxHeight="16" CodecPrivateData="000000016742000A0000000168CE"/>
    <c t="0" d="10000000"/>
  </StreamIndex>
  <StreamIndex Type="audio" TimeScale="48000" Chunks="1" QualityLevels="1" Url="q({bitrate})/a({start time})">
    <QualityLevel Index="1" Bitrate="96000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>
    <c t="48000" d="48000"/>
  </StreamIndex>
</SmoothStreamingMedia>"#;
    assert_eq!(
        parse(zero_overlap),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::NoCommonPlaybackInterval,
        })
    );
}

#[test]
fn inferred_start_duration_and_audio_without_asc_are_explicitly_modelled() {
    let manifest = parse(TIMELINE_INFERENCE).expect("adjacent explicit t выводит d");
    assert_eq!(
        manifest.duration().timescale().get(),
        SMOOTH_STREAMING_DEFAULT_TIMESCALE_TICKS_PER_SECOND
    );
    let stream = &manifest.streams()[0];
    assert_eq!(stream.name().unwrap().as_str(), "audio-main");
    assert_eq!(stream.language().unwrap().as_str(), "en-US");
    assert_eq!(stream.timeline().fragment_count(), 3);
    assert_eq!(
        stream.timeline().fragment_at(1).unwrap().start().ticks(),
        10
    );
    let SmoothQualityLevel::Audio(quality) = &stream.qualities()[0] else {
        panic!("fixture audio-only");
    };
    assert_eq!(quality.index().get(), 7);
    assert_eq!(
        quality.codec_configuration().origin(),
        SmoothCodecConfigurationOrigin::AacDerivedFromQualityFields
    );
    assert_eq!(quality.codec_configuration().as_bytes(), [0x11, 0x90]);
}

#[test]
fn hardened_xml_rejections_preserve_exact_source() {
    for (case, document) in [
        ("malformed", MALFORMED_XML),
        (
            "doctype",
            "<!DOCTYPE SmoothStreamingMedia><SmoothStreamingMedia/>",
        ),
        ("entity", EXTERNAL_ENTITY),
        (
            "prefix",
            r#"<x:SmoothStreamingMedia MajorVersion="2" MinorVersion="0" TimeScale="1" Duration="1"/>"#,
        ),
    ] {
        let error = parse(document).expect_err("malformed/security XML обязан остаться XML error");
        let SmoothManifestError::Xml { source } = error else {
            panic!("{case}: XML rejection не должен превращаться в schema error: {error:?}");
        };
        assert!(matches!(
            source,
            XmlReadError::MalformedXml
                | XmlReadError::DocTypeForbidden
                | XmlReadError::CustomEntityForbidden
                | XmlReadError::InvalidNamespace
        ));
    }
}

#[test]
fn private_namespaces_unknown_vocabulary_and_drm_are_distinct() {
    let private_element = valid_video_document("<x:Vendor/>", r#"xmlns:x="urn:private""#);
    assert_eq!(
        parse(&private_element),
        Err(SmoothManifestError::PrivateExtension)
    );
    let private_attribute = valid_video_document("", r#"xmlns:x="urn:private" x:mode="vendor""#);
    assert_eq!(
        parse(&private_attribute),
        Err(SmoothManifestError::PrivateExtension)
    );
    assert_eq!(
        parse(&valid_video_document("<Vendor/>", "")),
        Err(SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::UnknownElement,
        })
    );
    assert_eq!(
        parse(&valid_video_document("", r#"Unknown="value""#)),
        Err(SmoothManifestError::UnsupportedConstruct {
            construct: SmoothUnsupportedConstruct::UnknownAttribute,
        })
    );
    assert_eq!(parse(DRM_PLAYREADY), Err(SmoothManifestError::DrmProtected));
    assert_eq!(
        parse(&valid_video_document("<ProtectionHeader/>", "")),
        Err(SmoothManifestError::DrmProtected)
    );
}

#[test]
fn excluded_stream_shapes_are_typed_without_fallback() {
    for (stream_type, reason) in [
        ("text", SmoothProfileIncompatibility::TextStream),
        ("sparse", SmoothProfileIncompatibility::SparseStream),
        ("embedded", SmoothProfileIncompatibility::EmbeddedStream),
        ("composite", SmoothProfileIncompatibility::CompositeStream),
        ("trickmode", SmoothProfileIncompatibility::TrickModeStream),
    ] {
        let document =
            VALID_V22_REPEAT.replace(r#"Type="video""#, &format!(r#"Type="{stream_type}""#));
        assert_eq!(
            parse(&document),
            Err(SmoothManifestError::ProfileIncompatible { reason })
        );
    }
    let vendor_subtype =
        VALID_V22_REPEAT.replace(r#"Type="video""#, r#"Type="video" Subtype="VENDOR""#);
    assert_eq!(
        parse(&vendor_subtype),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::VendorExtension,
        })
    );
    let embedded_parent =
        VALID_V22_REPEAT.replace(r#"Type="video""#, r#"Type="video" ParentStreamIndex="0""#);
    assert_eq!(
        parse(&embedded_parent),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::EmbeddedStream,
        })
    );
}

#[test]
fn codec_hex_and_profile_fail_closed_with_typed_causes() {
    assert_codec_error(
        MALFORMED_CODEC_PRIVATE_DATA,
        SmoothCodecConfigurationError::OddHexLength,
    );
    assert_codec_error(
        &VALID_V20.replace(
            "000000016742001E0000000168CE06E2",
            "000000016742001Z0000000168CE06E2",
        ),
        SmoothCodecConfigurationError::InvalidHexDigit,
    );
    assert_codec_error(
        &VALID_V20.replace("000000016742001E0000000168CE06E2", "000000016742001E"),
        SmoothCodecConfigurationError::MissingH264PictureParameterSet,
    );
    assert_codec_error(
        &VALID_V20.replace("000000016742001E0000000168CE06E2", "0000000168CE06E2"),
        SmoothCodecConfigurationError::MissingH264SequenceParameterSet,
    );
    assert_codec_error(
        &VALID_V20.replace(
            "000000016742001E0000000168CE06E2",
            "000000016742001E000000016742001F0000000168CE06E2",
        ),
        SmoothCodecConfigurationError::DuplicateH264SequenceParameterSet,
    );
    assert_codec_error(
        &VALID_V20.replace("CodecPrivateData=\"1190\"", "CodecPrivateData=\"0990\""),
        SmoothCodecConfigurationError::AacObjectTypeMismatch,
    );
    assert_codec_error(
        &VALID_V20.replace("CodecPrivateData=\"1190\"", "CodecPrivateData=\"1210\""),
        SmoothCodecConfigurationError::AacSamplingRateMismatch,
    );
    assert_codec_error(
        &VALID_V20.replace("CodecPrivateData=\"1190\"", "CodecPrivateData=\"1188\""),
        SmoothCodecConfigurationError::AacChannelCountMismatch,
    );
    assert_eq!(
        parse(&VALID_V20.replace("FourCC=\"AACL\"", "FourCC=\"AACH\"")),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedAudioCodec,
        })
    );
    assert_eq!(
        parse(UNSUPPORTED_CODEC),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedVideoCodec,
        })
    );
    assert_eq!(
        parse(&VALID_V20.replace("AudioTag=\"255\"", "AudioTag=\"1\"")),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::UnsupportedAudioTag,
        })
    );
}

#[test]
fn repeat_lexical_domain_distinguishes_negative_and_full_u64_budget_failure() {
    assert_eq!(
        parse(NEGATIVE_REPEAT),
        Err(SmoothManifestError::InvalidTimeline {
            reason: SmoothTimelineError::NegativeRepeat,
        })
    );
    assert!(matches!(
        parse(&VALID_V22_REPEAT.replace("r=\"2\"", "r=\"18446744073709551615\"")),
        Err(SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::FragmentsPerStream,
            ..
        }) | Err(SmoothManifestError::InvalidTimeline {
            reason: SmoothTimelineError::ArithmeticOverflow,
        })
    ));
}

#[test]
fn url_grammar_counts_and_standard_vod_exclusions_fail_closed() {
    for invalid_url in [
        "https://example.invalid/{bitrate}/{start time}",
        "../{bitrate}/{start time}",
        "{bitrate}/{start time}?token=x",
        "{bitrate}/missing-start",
    ] {
        let document = VALID_V22_REPEAT.replace(
            "QualityLevels({bitrate})/Fragments(video={start_time})",
            invalid_url,
        );
        assert!(matches!(
            parse(&document),
            Err(SmoothManifestError::InvalidUrlTemplate { .. })
        ));
    }
    assert_eq!(
        parse(&VALID_V22_REPEAT.replace("Chunks=\"2\"", "Chunks=\"3\"")),
        Err(SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::FragmentCount,
            declared: 3,
            actual: 2,
        })
    );
    assert_eq!(
        parse(&VALID_V22_REPEAT.replace("QualityLevels=\"1\"", "QualityLevels=\"2\"")),
        Err(SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::QualityCount,
            declared: 2,
            actual: 1,
        })
    );
    assert_eq!(
        parse(&VALID_V22_REPEAT.replace("StreamIndexCount=\"1\"", "StreamIndexCount=\"2\"")),
        Err(SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::StreamCount,
            declared: 2,
            actual: 1,
        })
    );
    for (attribute, construct) in [
        (
            r#"LookAheadFragmentCount="1""#,
            SmoothUnsupportedConstruct::LookAheadFragments,
        ),
        (
            r#"DVRWindowLength="20""#,
            SmoothUnsupportedConstruct::DvrWindow,
        ),
    ] {
        let document = VALID_V22_REPEAT.replacen(
            r#"Duration="20""#,
            &format!(r#"Duration="20" {attribute}"#),
            1,
        );
        assert_eq!(
            parse(&document),
            Err(SmoothManifestError::UnsupportedConstruct { construct })
        );
    }
    let live = VALID_V22_REPEAT.replacen(r#"Duration="20""#, r#"Duration="20" IsLive="TRUE""#, 1);
    assert_eq!(
        parse(&live),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::LiveManifest,
        })
    );
}

#[test]
fn cancellation_is_observed_at_start_events_hex_and_final_timeline_validation() {
    let mut immediately = || true;
    assert_eq!(
        parse_vod_client_manifest_cancellable(request(VALID_V20), &mut immediately),
        Err(SmoothManifestError::Cancelled)
    );

    let mut event_polls = 0usize;
    let mut during_events = || {
        event_polls += 1;
        event_polls >= 4
    };
    assert_eq!(
        parse_vod_client_manifest_cancellable(request(VALID_V20), &mut during_events),
        Err(SmoothManifestError::Cancelled)
    );

    let long_hex = VALID_V20.replace(
        "000000016742001E0000000168CE06E2",
        &format!("0000000167{}0000000168CE06E2", "42".repeat(128)),
    );
    let mut hex_polls = 0usize;
    let mut during_hex = || {
        hex_polls += 1;
        hex_polls >= 40
    };
    assert_eq!(
        parse_vod_client_manifest_cancellable(request(&long_hex), &mut during_hex),
        Err(SmoothManifestError::Cancelled)
    );

    let mut total_polls = 0usize;
    parse_vod_client_manifest_cancellable(request(VALID_V22_REPEAT), &mut || {
        total_polls += 1;
        false
    })
    .expect("baseline считает polls");
    let mut final_polls = 0usize;
    let mut before_publication = || {
        final_polls += 1;
        final_polls == total_polls
    };
    assert_eq!(
        parse_vod_client_manifest_cancellable(request(VALID_V22_REPEAT), &mut before_publication),
        Err(SmoothManifestError::Cancelled)
    );
}

/// Проверяет exact typed codec cause без раскрытия payload.
fn assert_codec_error(document: &str, expected: SmoothCodecConfigurationError) {
    assert_eq!(
        parse(document),
        Err(SmoothManifestError::InvalidCodecConfiguration { reason: expected })
    );
}

/// Строит минимальный valid root/stream и вставляет root child для taxonomy tests.
fn valid_video_document(root_child: &str, root_attributes: &str) -> String {
    format!(
        r#"<SmoothStreamingMedia MajorVersion="2" MinorVersion="0" TimeScale="10" Duration="10" {root_attributes}>
{root_child}
<StreamIndex Type="video" Chunks="1" QualityLevels="1" Url="q({{bitrate}})/v({{start time}})">
  <QualityLevel Index="0" Bitrate="1000" FourCC="H264" MaxWidth="16" MaxHeight="16" CodecPrivateData="000000016742000A0000000168CE"/>
  <c t="0" d="10"/>
</StreamIndex>
</SmoothStreamingMedia>"#
    )
}

/// Строит две H.264 quality rows для exact index/render-identity proofs.
fn two_quality_document(
    url_template: &str,
    second_index: u64,
    second_bitrate: u64,
    first_attribute: Option<(&str, &str)>,
    second_attribute: Option<(&str, &str)>,
) -> String {
    let first_quality = quality_row_with_attribute(0, 1_000, first_attribute);
    let second_quality = quality_row_with_attribute(second_index, second_bitrate, second_attribute);
    format!(
        r#"<SmoothStreamingMedia MajorVersion="2" MinorVersion="0" Duration="10000000">
<StreamIndex Type="video" Chunks="1" QualityLevels="2" Url="{url_template}">
  {first_quality}
  {second_quality}
  <c t="0" d="10000000"/>
</StreamIndex>
</SmoothStreamingMedia>"#
    )
}

/// Добавляет standard CustomAttributes subtree только когда fixture его запросил.
fn quality_row_with_attribute(index: u64, bitrate: u64, attribute: Option<(&str, &str)>) -> String {
    let opening = format!(
        r#"<QualityLevel Index="{index}" Bitrate="{bitrate}" FourCC="H264" MaxWidth="16" MaxHeight="16" CodecPrivateData="000000016742000A0000000168CE""#
    );
    match attribute {
        Some((name, value)) => format!(
            r#"{opening}><CustomAttributes><Attribute Name="{name}" Value="{value}"/></CustomAttributes></QualityLevel>"#
        ),
        None => format!("{opening}/>"),
    }
}
