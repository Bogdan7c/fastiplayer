use super::*;
use crate::{
    AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity, DynamicRange,
    ExtractionGeneration, NormalizedCodec, RawCodecIdentity, SemanticIdentity, VideoHeight,
};

pub(super) fn parent(source_value: u64, format: &str, semantic: &str) -> ExactSelectionIdentity {
    parent_at_generation(source_value, 7, format, semantic)
}

pub(super) fn parent_at_generation(
    source_value: u64,
    extraction_generation: u64,
    format: &str,
    semantic: &str,
) -> ExactSelectionIdentity {
    let source = SourceIdentity::new(source_value);
    ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(extraction_generation),
            CandidateFormatIdentity::new(format).expect("test format identity должна быть valid"),
        ),
        SemanticIdentity::new(source, semantic).expect("test semantic identity должна быть valid"),
    )
    .expect("test parent identities должны иметь один source")
}

pub(super) fn catalog_identity(
    parent: ExactSelectionIdentity,
    generation: u64,
) -> ComponentVariantCatalogIdentity {
    ComponentVariantCatalogIdentity::new(parent, ComponentVariantCatalogGeneration::new(generation))
}

pub(super) fn video_track(height: Option<u32>) -> VideoTrackDescriptor {
    VideoTrackDescriptor::new(
        NormalizedCodec::parse(
            RawCodecIdentity::new("vp9").expect("test video codec identity должна быть valid"),
        ),
        None,
        height.map(|pixels| VideoHeight::new(pixels).expect("test height должна быть valid")),
        None,
        None,
        DynamicRange::Sdr,
    )
}

pub(super) fn audio_track(bitrate_marker: u32) -> AudioTrackDescriptor {
    let codec = if bitrate_marker.is_multiple_of(2) {
        "opus"
    } else {
        "aac"
    };
    AudioTrackDescriptor::new(
        NormalizedCodec::parse(
            RawCodecIdentity::new(codec).expect("test audio codec identity должна быть valid"),
        ),
        None,
        None,
        None,
        None,
    )
}

pub(super) fn video_variant(
    catalog: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    height: Option<u32>,
) -> VideoComponentVariant {
    VideoComponentVariant::new(
        ComponentVariantExactIdentity::new(
            catalog.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new(exact_key).expect("test exact key должен быть valid"),
        ),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new(semantic_key)
                .expect("test semantic key должен быть valid"),
        ),
        video_track(height),
    )
}

pub(super) fn audio_variant(
    catalog: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    marker: u32,
) -> AudioComponentVariant {
    AudioComponentVariant::new(
        ComponentVariantExactIdentity::new(
            catalog.clone(),
            ComponentKind::Audio,
            ComponentVariantExactKey::new(exact_key).expect("test exact key должен быть valid"),
        ),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            ComponentKind::Audio,
            ComponentVariantSemanticKey::new(semantic_key)
                .expect("test semantic key должен быть valid"),
        ),
        audio_track(marker),
    )
}

pub(super) fn generous_limit() -> ComponentVariantCatalogLimit {
    ComponentVariantCatalogLimit::new(32).expect("test limit должен быть valid")
}

pub(super) fn video_and_audio_catalog() -> ComponentVariantCatalog {
    let identity = catalog_identity(parent(1, "parent", "parent-semantic"), 3);
    ComponentVariantCatalog::new(
        identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![
                video_variant(&identity, "video-720", "video-semantic-720", Some(720)),
                video_variant(&identity, "video-1080", "video-semantic-1080", Some(1080)),
            ],
            audio: vec![
                audio_variant(&identity, "audio-a", "audio-semantic-a", 1),
                audio_variant(&identity, "audio-b", "audio-semantic-b", 2),
            ],
        },
    )
    .expect("test catalog должен быть valid")
}
