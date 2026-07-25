use crate::{
    SmoothAudioQuality, SmoothChunkDuration, SmoothChunkEntry, SmoothChunkRepeat, SmoothChunkStart,
    SmoothCodecConfiguration, SmoothCodecConfigurationOrigin, SmoothCodecFourCc,
    SmoothCustomAttribute, SmoothCustomAttributeName, SmoothCustomAttributeSet,
    SmoothCustomAttributeValue, SmoothDeclaredCountKind, SmoothDeclaredFragmentCount,
    SmoothDeclaredQualityCount, SmoothDeclaredStreamCount, SmoothFragmentUrlTemplate,
    SmoothManifest, SmoothManifestError, SmoothManifestLimitKind, SmoothManifestTimelineBudget,
    SmoothManifestVersion, SmoothProfileIncompatibility, SmoothQualityIndex, SmoothQualityLevel,
    SmoothStream, SmoothStreamConstruction, SmoothStreamIdentityMetadata, SmoothStreamKind,
    SmoothTime, SmoothTimescale, SmoothVideoQuality,
};

use crate::tests_support::{limits, limits_builder};

#[test]
fn version_vocabulary_accepts_only_exact_20_and_22() {
    assert_eq!(
        SmoothManifestVersion::from_major_minor(2, 0),
        Ok(SmoothManifestVersion::V2_0)
    );
    assert_eq!(
        SmoothManifestVersion::from_major_minor(2, 2),
        Ok(SmoothManifestVersion::V2_2)
    );
    assert_eq!(
        SmoothManifestVersion::from_major_minor(2, 1),
        Err(SmoothManifestError::UnsupportedVersion { major: 2, minor: 1 })
    );
}

#[test]
fn codec_configuration_preserves_validated_origin_and_bytes() {
    let proven = SmoothCodecConfiguration::from_validated(
        Box::<[u8]>::from([0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x68]),
        SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets,
    );
    assert_eq!(
        proven.origin(),
        SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets
    );
    assert_eq!(proven.as_bytes().len(), 10);
}

#[test]
fn custom_attributes_reject_duplicates_unsafe_atoms_and_count_overflow() {
    let configured_limits = limits_builder()
        .maximum_custom_attributes_per_quality(1)
        .build()
        .expect("test limits валидны");
    let first = custom_attribute("lang", "en-US", &configured_limits);
    let duplicate = custom_attribute("lang", "uk-UA", &configured_limits);
    assert_eq!(
        SmoothCustomAttributeSet::new(vec![first.clone(), duplicate], &configured_limits),
        Err(SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::CustomAttributesPerQuality,
            maximum: 1,
        })
    );

    let room_for_duplicates = limits();
    assert_eq!(
        SmoothCustomAttributeSet::new(
            vec![
                custom_attribute("lang", "en-US", &room_for_duplicates),
                custom_attribute("lang", "uk-UA", &room_for_duplicates),
            ],
            &room_for_duplicates,
        ),
        Err(SmoothManifestError::MalformedSchema {
            field: crate::SmoothSchemaField::QualityLevel,
        })
    );
    assert!(SmoothCustomAttributeName::new("../lang", &room_for_duplicates).is_err());
    assert!(SmoothCustomAttributeValue::new("main/value", &room_for_duplicates).is_err());
}

#[test]
fn stream_rejects_mixed_quality_axis_timescale_and_declared_count_mismatch() {
    let configured_limits = limits();
    let video = SmoothQualityLevel::Video(video_quality(&configured_limits));
    let audio = SmoothQualityLevel::Audio(audio_quality(&configured_limits));
    let video_clock = SmoothTimescale::new(10_000_000).expect("timescale валиден");
    let audio_clock = SmoothTimescale::new(48_000).expect("timescale валиден");

    let mixed_error = SmoothStream::new(
        SmoothStreamConstruction {
            kind: SmoothStreamKind::Video,
            identity_metadata: SmoothStreamIdentityMetadata::new(None, None),
            timescale: video_clock,
            url_template: template(&configured_limits),
            qualities: vec![video.clone(), audio],
            timeline: timeline(video_clock, &configured_limits),
            declared_quality_count: SmoothDeclaredQualityCount::Unspecified,
        },
        &configured_limits,
    )
    .expect_err("mixed axes должны быть невозможны");
    assert_eq!(
        mixed_error,
        SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::MixedQualityKinds,
        }
    );

    let timescale_error = SmoothStream::new(
        SmoothStreamConstruction {
            kind: SmoothStreamKind::Video,
            identity_metadata: SmoothStreamIdentityMetadata::new(None, None),
            timescale: audio_clock,
            url_template: template(&configured_limits),
            qualities: vec![video.clone()],
            timeline: timeline(video_clock, &configured_limits),
            declared_quality_count: SmoothDeclaredQualityCount::Unspecified,
        },
        &configured_limits,
    )
    .expect_err("stream и timeline clocks должны совпасть");
    assert_eq!(
        timescale_error,
        SmoothManifestError::MalformedSchema {
            field: crate::SmoothSchemaField::Timeline,
        }
    );

    let count_error = SmoothStream::new(
        SmoothStreamConstruction {
            kind: SmoothStreamKind::Video,
            identity_metadata: SmoothStreamIdentityMetadata::new(None, None),
            timescale: video_clock,
            url_template: template(&configured_limits),
            qualities: vec![video],
            timeline: timeline(video_clock, &configured_limits),
            declared_quality_count: SmoothDeclaredQualityCount::Exact(2),
        },
        &configured_limits,
    )
    .expect_err("declared QualityLevels обязан совпасть");
    assert_eq!(
        count_error,
        SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::QualityCount,
            declared: 2,
            actual: 1,
        }
    );
}

#[test]
fn manifest_prevents_empty_streams_total_quality_overflow_and_count_mismatch() {
    let configured_limits = limits();
    let duration_clock = SmoothTimescale::new(10_000_000).expect("timescale валиден");
    let duration = SmoothTime::new(20_000_000, duration_clock);
    assert_eq!(
        SmoothManifest::new_vod(
            SmoothManifestVersion::V2_0,
            duration,
            Vec::new(),
            SmoothDeclaredStreamCount::Unspecified,
            &configured_limits,
        ),
        Err(SmoothManifestError::ProfileIncompatible {
            reason: SmoothProfileIncompatibility::MissingRequiredStream,
        })
    );

    let stream = video_stream(&configured_limits);
    let count_error = SmoothManifest::new_vod(
        SmoothManifestVersion::V2_0,
        duration,
        vec![stream.clone()],
        SmoothDeclaredStreamCount::Exact(2),
        &configured_limits,
    )
    .expect_err("declared StreamIndex count обязан совпасть");
    assert_eq!(
        count_error,
        SmoothManifestError::DeclaredCountMismatch {
            kind: SmoothDeclaredCountKind::StreamCount,
            declared: 2,
            actual: 1,
        }
    );

    let tight_limits = limits_builder()
        .maximum_qualities_per_stream(2)
        .maximum_total_qualities(2)
        .build()
        .expect("test limits валидны");
    let total_error = SmoothManifest::new_vod(
        SmoothManifestVersion::V2_0,
        duration,
        vec![
            video_stream(&tight_limits),
            video_stream(&tight_limits),
            video_stream(&tight_limits),
        ],
        SmoothDeclaredStreamCount::Unspecified,
        &tight_limits,
    )
    .expect_err("total quality budget должен примениться");
    assert_eq!(
        total_error,
        SmoothManifestError::LimitExceeded {
            limit: SmoothManifestLimitKind::TotalQualities,
            maximum: 2,
        }
    );
}

#[test]
fn model_and_error_debug_are_secret_safe() {
    let secret = b"secret-codec-private-token".as_slice();
    let configuration = SmoothCodecConfiguration::from_validated(
        secret.into(),
        SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets,
    );
    assert!(!format!("{configuration:?}").contains("secret"));
}

fn video_stream(limits: &crate::SmoothManifestLimits) -> SmoothStream {
    let timescale = SmoothTimescale::new(10_000_000).expect("timescale валиден");
    SmoothStream::new(
        SmoothStreamConstruction {
            kind: SmoothStreamKind::Video,
            identity_metadata: SmoothStreamIdentityMetadata::new(None, None),
            timescale,
            url_template: template(limits),
            qualities: vec![SmoothQualityLevel::Video(video_quality(limits))],
            timeline: timeline(timescale, limits),
            declared_quality_count: SmoothDeclaredQualityCount::Exact(1),
        },
        limits,
    )
    .expect("video stream валиден")
}

fn video_quality(limits: &crate::SmoothManifestLimits) -> SmoothVideoQuality {
    SmoothVideoQuality::new(
        SmoothQualityIndex::new(0),
        1_500_000,
        1_920,
        1_080,
        SmoothCodecFourCc::new_validated("H264", limits).expect("FourCC валиден"),
        SmoothCodecConfiguration::from_validated(
            Box::<[u8]>::from([0, 0, 0, 1, 0x67, 0, 0, 0, 1, 0x68]),
            SmoothCodecConfigurationOrigin::H264SequenceAndPictureParameterSets,
        ),
        SmoothCustomAttributeSet::new(Vec::new(), limits).expect("empty attrs валидны"),
    )
    .expect("video quality валидна")
}

fn audio_quality(limits: &crate::SmoothManifestLimits) -> SmoothAudioQuality {
    SmoothAudioQuality::new(
        SmoothQualityIndex::new(0),
        128_000,
        48_000,
        2,
        16,
        4,
        255,
        SmoothCodecFourCc::new_validated("AACL", limits).expect("FourCC валиден"),
        SmoothCodecConfiguration::from_validated(
            Box::<[u8]>::from([0x11, 0x90]),
            SmoothCodecConfigurationOrigin::AacAudioSpecificConfig,
        ),
        SmoothCustomAttributeSet::new(Vec::new(), limits).expect("empty attrs валидны"),
    )
    .expect("audio quality валидна")
}

fn template(limits: &crate::SmoothManifestLimits) -> SmoothFragmentUrlTemplate {
    SmoothFragmentUrlTemplate::parse(
        "QualityLevels({bitrate})/Fragments(video={start time})",
        limits,
    )
    .expect("template валиден")
}

fn timeline(
    timescale: SmoothTimescale,
    limits: &crate::SmoothManifestLimits,
) -> crate::SmoothChunkTimeline {
    let mut budget = SmoothManifestTimelineBudget::new(limits);
    budget
        .build_stream_timeline(
            SmoothManifestVersion::V2_0,
            timescale,
            &[SmoothChunkEntry::new(
                SmoothChunkStart::Inferred,
                SmoothChunkDuration::Explicit(10),
                SmoothChunkRepeat::ImplicitSingle,
            )],
            SmoothDeclaredFragmentCount::Exact(1),
        )
        .expect("timeline валиден")
}

fn custom_attribute(
    name: &str,
    value: &str,
    limits: &crate::SmoothManifestLimits,
) -> SmoothCustomAttribute {
    SmoothCustomAttribute::new(
        SmoothCustomAttributeName::new(name, limits).expect("name валидно"),
        SmoothCustomAttributeValue::new(value, limits).expect("value валидно"),
    )
}
