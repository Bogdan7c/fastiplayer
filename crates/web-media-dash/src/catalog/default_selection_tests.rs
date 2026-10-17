//! Native DASH default: AC-3/E-AC-3 audio не вытесняет прежние families.

use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity,
    ChannelCount, CodecFamily, CodecKind, ComponentKind, ComponentVariantCatalog,
    ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEntries, ComponentVariantEdgeLimit, ComponentVariantExactIdentity,
    ComponentVariantExactKey, ComponentVariantSelection, ComponentVariantSemanticIdentity,
    ComponentVariantSemanticKey, DynamicRange, ExactSelectionIdentity, ExtractionGeneration,
    NormalizedCodec, PreferredHeightPolicy, RawCodecIdentity, SemanticIdentity, SourceIdentity,
    VideoComponentVariant, VideoHeight, VideoTrackDescriptor,
};

use super::native_provider_default_selection;

fn catalog_identity() -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(5);
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(1),
            CandidateFormatIdentity::new("native-dash").expect("format identity"),
        ),
        SemanticIdentity::new(source, "native-dash").expect("semantic identity"),
    )
    .expect("one source lineage");
    ComponentVariantCatalogIdentity::new(parent, ComponentVariantCatalogGeneration::new(1))
}

fn exact(
    identity: &ComponentVariantCatalogIdentity,
    kind: ComponentKind,
    key: &str,
) -> ComponentVariantExactIdentity {
    ComponentVariantExactIdentity::new(
        identity.clone(),
        kind,
        ComponentVariantExactKey::new(key).expect("exact key"),
    )
}

fn semantic(
    identity: &ComponentVariantCatalogIdentity,
    kind: ComponentKind,
    key: &str,
) -> ComponentVariantSemanticIdentity {
    ComponentVariantSemanticIdentity::new(
        identity.parent().semantic().clone(),
        kind,
        ComponentVariantSemanticKey::new(key).expect("semantic key"),
    )
}

fn video(identity: &ComponentVariantCatalogIdentity) -> VideoComponentVariant {
    VideoComponentVariant::new(
        exact(identity, ComponentKind::Video, "video-720"),
        semantic(identity, ComponentKind::Video, "video-720"),
        VideoTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("avc1.64001f").expect("codec")),
            None,
            Some(VideoHeight::new(720).expect("height")),
            None,
            None,
            DynamicRange::Sdr,
        ),
    )
}

fn audio(
    identity: &ComponentVariantCatalogIdentity,
    key: &str,
    codec: &str,
    channels: u16,
) -> AudioComponentVariant {
    AudioComponentVariant::new(
        exact(identity, ComponentKind::Audio, key),
        semantic(identity, ComponentKind::Audio, key),
        AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new(codec).expect("codec")),
            None,
            Some(ChannelCount::new(channels).expect("channels")),
            None,
            None,
        ),
    )
}

/// Catalog с одним video и audio pool-ом в заданном (MPD) порядке.
fn catalog(with_video: bool, audio_rows: &[(&str, &str, u16)]) -> ComponentVariantCatalog {
    let identity = catalog_identity();
    let audio_variants = audio_rows
        .iter()
        .map(|(key, codec, channels)| audio(&identity, key, codec, *channels))
        .collect::<Vec<_>>();
    let audio_only = audio_variants
        .iter()
        .map(|variant| variant.exact_identity().clone())
        .collect();
    let (video, compatibility) = if with_video {
        (
            vec![video(&identity)],
            ComponentVariantCompatibilityEntries::AllPairs {
                edge_limit: ComponentVariantEdgeLimit::new(16).expect("edge limit"),
            },
        )
    } else {
        (
            Vec::new(),
            ComponentVariantCompatibilityEntries::Unavailable,
        )
    };
    ComponentVariantCatalog::new(
        identity,
        ComponentVariantCatalogLimit::new(16).expect("catalog limit"),
        ComponentVariantCatalogEntries::Topology {
            video,
            audio: audio_variants,
            compatibility,
            coupled: Vec::new(),
            video_only: Vec::new(),
            audio_only,
        },
    )
    .expect("valid native DASH catalog")
}

/// Codec family выбранного audio (separate или audio-only selection).
fn selected_audio_family(selection: &ComponentVariantSelection) -> CodecKind {
    match selection {
        ComponentVariantSelection::VideoAndAudio { audio, .. }
        | ComponentVariantSelection::AudioOnly { audio, .. } => audio.track().codec().kind(),
        unexpected => panic!("audio selection expected, got {unexpected:?}"),
    }
}

/// E-AC-3 5.1 раньше AAC в MPD: default всё равно остаётся AAC, как до поддержки AC-3.
#[test]
fn separate_default_keeps_primary_audio_even_when_dolby_is_first() {
    let catalog = catalog(true, &[("eac3-51", "ec-3", 6), ("aac-20", "mp4a.40.2", 2)]);
    let selection =
        native_provider_default_selection(&catalog, PreferredHeightPolicy::NoPreference)
            .expect("separate default must exist");
    assert!(matches!(
        selection,
        ComponentVariantSelection::VideoAndAudio { .. }
    ));
    assert_eq!(
        selected_audio_family(&selection),
        CodecKind::Known(CodecFamily::Aac)
    );
}

/// Когда кроме Dolby audio ничего нет, default выбирает его (раньше MPD не открывался).
#[test]
fn dolby_audio_is_selected_when_it_is_the_only_playable_audio() {
    let separate = catalog(true, &[("eac3-51", "ec-3", 6)]);
    let selection =
        native_provider_default_selection(&separate, PreferredHeightPolicy::NoPreference)
            .expect("Dolby-only separate default must exist");
    assert_eq!(
        selected_audio_family(&selection),
        CodecKind::Known(CodecFamily::Eac3)
    );

    let audio_only = catalog(false, &[("ac3-20", "ac-3", 2), ("eac3-51", "ec-3", 6)]);
    let selection =
        native_provider_default_selection(&audio_only, PreferredHeightPolicy::NoPreference)
            .expect("Dolby-only audio default must exist");
    // Внутри одного tier-а сохраняется прежнее правило «первый подходящий».
    assert_eq!(
        selected_audio_family(&selection),
        CodecKind::Known(CodecFamily::Ac3)
    );
}

/// Audio-only fallback тоже предпочитает прежнюю family.
#[test]
fn audio_only_default_keeps_primary_audio_even_when_dolby_is_first() {
    let catalog = catalog(false, &[("eac3-51", "ec-3", 6), ("aac-20", "mp4a.40.2", 2)]);
    let selection =
        native_provider_default_selection(&catalog, PreferredHeightPolicy::NoPreference)
            .expect("audio-only default must exist");
    assert_eq!(
        selected_audio_family(&selection),
        CodecKind::Known(CodecFamily::Aac)
    );
}
