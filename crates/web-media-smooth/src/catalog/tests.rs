use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use web_media_core::{ComponentVariantCatalog, PreferredHeightPolicy};

use super::{SmoothCatalogBuildRequest, build_catalog, canonical_audio_key, canonical_video_key};
use crate::SmoothProfileError;
use crate::test_support::{VALID_MANIFEST, catalog_identity, parse, policy};

#[test]
fn builds_init_for_every_quality_and_sorts_axes_deterministically() {
    let document = VALID_MANIFEST
        .replace("QualityLevels=\"1\"", "QualityLevels=\"2\"")
        .replacen(
            r#"</QualityLevel>"#,
            r#"</QualityLevel>
    <QualityLevel Index="1" Bitrate="750000" FourCC="H264" MaxWidth="1280" MaxHeight="720" CodecPrivateData="000000016742001E0000000168CE06E2"/>"#,
            1,
        )
        .replacen(
            r#"<QualityLevel Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>"#,
            r#"<QualityLevel Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>
    <QualityLevel Index="1" Bitrate="64000" FourCC="AACL" SamplingRate="48000" Channels="2" BitsPerSample="16" PacketSize="4" AudioTag="255" CodecPrivateData="1190"/>"#,
            1,
        );
    let manifest = Arc::new(parse(&document));
    let (identity, semantic) = catalog_identity();
    let policy = policy(64 * 1024);
    let built = build_catalog(SmoothCatalogBuildRequest {
        manifest: &manifest,
        catalog_identity: identity,
        parent_semantic: &semantic,
        video_stream_ordinal: 0,
        audio_stream_ordinal: 1,
        preferred_height: PreferredHeightPolicy::NoPreference,
        policy: &policy,
        cancellation: &|| false,
    })
    .expect("все качества обязаны материализоваться");

    assert_eq!(built.video_rows.len(), 2);
    assert_eq!(built.audio_rows.len(), 2);
    assert!(matches!(
        built.catalog,
        ComponentVariantCatalog::Topology { .. }
    ));
    assert_eq!(
        built.video_rows[0].selection.quality_index.get(),
        0,
        "1080p row должна быть первой"
    );
    assert_eq!(
        built.audio_rows[0].selection.quality_index.get(),
        0,
        "128 kbps row должна быть первой"
    );
    assert!(
        built
            .video_rows
            .iter()
            .chain(built.audio_rows.iter())
            .all(|row| !row.initialization_bytes.is_empty())
    );
}

#[test]
fn aggregate_initialization_budget_fails_whole_catalog() {
    let manifest = Arc::new(parse(VALID_MANIFEST));
    let (identity, semantic) = catalog_identity();

    assert!(matches!(
        build_catalog(SmoothCatalogBuildRequest {
            manifest: &manifest,
            catalog_identity: identity,
            parent_semantic: &semantic,
            video_stream_ordinal: 0,
            audio_stream_ordinal: 1,
            preferred_height: PreferredHeightPolicy::NoPreference,
            policy: &policy(1),
            cancellation: &|| false,
        }),
        Err(crate::SmoothPrepareError::Profile(
            SmoothProfileError::AggregateInitializationLimit
        ))
    ));
}

#[test]
fn canonical_keys_ignore_declared_index_and_xml_attribute_order() {
    let reordered = VALID_MANIFEST
        .replace(
            r#"Index="0" Bitrate="1500000" FourCC="H264" MaxWidth="1920" MaxHeight="1080""#,
            r#"MaxHeight="1080" FourCC="H264" Index="91" MaxWidth="1920" Bitrate="1500000""#,
        )
        .replace(
            r#"Index="0" Bitrate="128000" FourCC="AACL" SamplingRate="48000" Channels="2""#,
            r#"Channels="2" SamplingRate="48000" Index="92" FourCC="AACL" Bitrate="128000""#,
        );
    let original = parse(VALID_MANIFEST);
    let reordered = parse(&reordered);
    let original_video = match &original.streams()[0].qualities()[0] {
        smooth_streaming_manifest_core::SmoothQualityLevel::Video(value) => value,
        _ => panic!("video"),
    };
    let reordered_video = match &reordered.streams()[0].qualities()[0] {
        smooth_streaming_manifest_core::SmoothQualityLevel::Video(value) => value,
        _ => panic!("video"),
    };
    let original_audio = match &original.streams()[1].qualities()[0] {
        smooth_streaming_manifest_core::SmoothQualityLevel::Audio(value) => value,
        _ => panic!("audio"),
    };
    let reordered_audio = match &reordered.streams()[1].qualities()[0] {
        smooth_streaming_manifest_core::SmoothQualityLevel::Audio(value) => value,
        _ => panic!("audio"),
    };

    let original_video_key = canonical_video_key(original_video).expect("video key framing");
    let reordered_video_key =
        canonical_video_key(reordered_video).expect("reordered video key framing");
    let original_audio_key = canonical_audio_key(original_audio, None).expect("audio key framing");
    let reordered_audio_key =
        canonical_audio_key(reordered_audio, None).expect("reordered audio key framing");

    assert_eq!(original_video_key, reordered_video_key);
    assert_eq!(original_audio_key, reordered_audio_key);
    assert!(original_video_key.starts_with("ss-v1-v-"));
    assert!(original_audio_key.starts_with("ss-v1-a-"));
    assert_eq!(original_video_key.len(), 72);
    assert_eq!(original_audio_key.len(), 72);
}

#[test]
fn cancellation_collapses_before_partial_catalog_publication() {
    let manifest = Arc::new(parse(VALID_MANIFEST));
    let (identity, semantic) = catalog_identity();

    assert!(matches!(
        build_catalog(SmoothCatalogBuildRequest {
            manifest: &manifest,
            catalog_identity: identity,
            parent_semantic: &semantic,
            video_stream_ordinal: 0,
            audio_stream_ordinal: 1,
            preferred_height: PreferredHeightPolicy::NoPreference,
            policy: &policy(64 * 1024),
            cancellation: &|| true,
        }),
        Err(crate::SmoothPrepareError::Cancelled)
    ));
}

#[test]
fn cancellation_at_final_publication_fence_drops_complete_catalog() {
    let manifest = Arc::new(parse(VALID_MANIFEST));
    let policy = policy(64 * 1024);
    let successful_call_count = AtomicUsize::new(0);
    let (identity, semantic) = catalog_identity();
    build_catalog(SmoothCatalogBuildRequest {
        manifest: &manifest,
        catalog_identity: identity,
        parent_semantic: &semantic,
        video_stream_ordinal: 0,
        audio_stream_ordinal: 1,
        preferred_height: PreferredHeightPolicy::NoPreference,
        policy: &policy,
        cancellation: &|| {
            successful_call_count.fetch_add(1, Ordering::SeqCst);
            false
        },
    })
    .expect("baseline catalog");
    let total_successful_checks = successful_call_count.load(Ordering::SeqCst);
    let final_check_index = total_successful_checks
        .checked_sub(1)
        .expect("catalog обязан иметь final publication fence");

    let cancelled_call_count = AtomicUsize::new(0);
    let (identity, semantic) = catalog_identity();
    let result = build_catalog(SmoothCatalogBuildRequest {
        manifest: &manifest,
        catalog_identity: identity,
        parent_semantic: &semantic,
        video_stream_ordinal: 0,
        audio_stream_ordinal: 1,
        preferred_height: PreferredHeightPolicy::NoPreference,
        policy: &policy,
        cancellation: &|| cancelled_call_count.fetch_add(1, Ordering::SeqCst) >= final_check_index,
    });

    assert!(matches!(result, Err(crate::SmoothPrepareError::Cancelled)));
    assert_eq!(
        cancelled_call_count.load(Ordering::SeqCst),
        total_successful_checks,
        "cancellation должна сработать только на последнем publication fence"
    );
}
