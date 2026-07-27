use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, CandidateFormatIdentity, CandidateIdentity,
    ComponentKind, ComponentVariantCatalog, ComponentVariantCatalogEntries,
    ComponentVariantCatalogGeneration, ComponentVariantCatalogIdentity,
    ComponentVariantCatalogLimit, ComponentVariantCompatibilityEntries, ComponentVariantEdgeLimit,
    ComponentVariantExactIdentity, ComponentVariantExactKey, ComponentVariantSelection,
    ComponentVariantSemanticIdentity, ComponentVariantSemanticKey, DynamicRange,
    ExactSelectionIdentity, ExtractionGeneration, NormalizedCodec, RawCodecIdentity,
    SemanticIdentity, SourceIdentity, VideoComponentVariant, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};

use super::{limit_parent_choices, visit_provider_selections};
use crate::web_media_catalog::{WebMediaCatalogChoice, WebMediaMode, WebMediaSelectionTarget};

fn fixture_choice(target: u64, rank: usize, mode: WebMediaMode) -> WebMediaCatalogChoice {
    WebMediaCatalogChoice {
        mode,
        video: None,
        rank: web_media_playback_plan::OpaqueAlternativeRank::parent(rank),
        target: WebMediaSelectionTarget::Fixture(target),
    }
}

fn catalog_identity() -> ComponentVariantCatalogIdentity {
    let source = SourceIdentity::new(91);
    let parent = ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(7),
            CandidateFormatIdentity::new("parent").unwrap(),
        ),
        SemanticIdentity::new(source, "parent-semantic").unwrap(),
    )
    .unwrap();
    ComponentVariantCatalogIdentity::new(parent, ComponentVariantCatalogGeneration::new(3))
}

fn video_variant(catalog: &ComponentVariantCatalogIdentity, key: &str) -> VideoComponentVariant {
    VideoComponentVariant::new(
        ComponentVariantExactIdentity::new(
            catalog.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new(key).unwrap(),
        ),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new(format!("semantic-{key}")).unwrap(),
        ),
        VideoTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("avc1.640028").unwrap()),
            Some(VideoWidth::new(1_920).unwrap()),
            Some(VideoHeight::new(1_080).unwrap()),
            None,
            None,
            DynamicRange::Sdr,
        ),
    )
}

fn audio_variant(catalog: &ComponentVariantCatalogIdentity, key: &str) -> AudioComponentVariant {
    AudioComponentVariant::new(
        ComponentVariantExactIdentity::new(
            catalog.clone(),
            ComponentKind::Audio,
            ComponentVariantExactKey::new(key).unwrap(),
        ),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            ComponentKind::Audio,
            ComponentVariantSemanticKey::new(format!("semantic-{key}")).unwrap(),
        ),
        AudioTrackDescriptor::new(
            NormalizedCodec::parse(RawCodecIdentity::new("mp4a.40.2").unwrap()),
            None,
            None,
            None,
            None,
        ),
    )
}

fn collect_selections(
    catalog: &ComponentVariantCatalog,
) -> Vec<(WebMediaMode, ComponentVariantSelection)> {
    let mut selections = Vec::new();
    visit_provider_selections(catalog, |mode, _video, selection| {
        selections.push((mode, selection));
        Ok(())
    })
    .unwrap();
    selections
}

#[test]
fn provider_projection_keeps_every_compatible_audio_row() {
    let identity = catalog_identity();
    let video = video_variant(&identity, "video");
    let audio_a = audio_variant(&identity, "audio-a");
    let audio_b = audio_variant(&identity, "audio-b");
    let audio_identities = [
        audio_a.exact_identity().clone(),
        audio_b.exact_identity().clone(),
    ];
    let catalog = ComponentVariantCatalog::new(
        identity,
        ComponentVariantCatalogLimit::new(8).unwrap(),
        ComponentVariantCatalogEntries::Topology {
            video: vec![video],
            audio: vec![audio_b, audio_a],
            compatibility: ComponentVariantCompatibilityEntries::AllPairs {
                edge_limit: ComponentVariantEdgeLimit::new(2).unwrap(),
            },
            coupled: Vec::new(),
            video_only: Vec::new(),
            audio_only: Vec::new(),
        },
    )
    .unwrap();

    let selections = collect_selections(&catalog);
    assert_eq!(selections.len(), 2);
    for audio_identity in audio_identities {
        assert!(selections.iter().any(|(mode, selection)| {
            *mode == WebMediaMode::VideoAndAudio
                && matches!(
                    selection,
                    ComponentVariantSelection::VideoAndAudio { audio, .. }
                        if audio.exact_identity() == &audio_identity
                )
        }));
    }
}

#[test]
fn provider_projection_keeps_standalone_single_component_catalogs() {
    let video_identity = catalog_identity();
    let video_catalog = ComponentVariantCatalog::new(
        video_identity.clone(),
        ComponentVariantCatalogLimit::new(2).unwrap(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(&video_identity, "video-only")],
        },
    )
    .unwrap();
    assert!(matches!(
        collect_selections(&video_catalog).as_slice(),
        [(
            WebMediaMode::VideoOnly,
            ComponentVariantSelection::VideoOnly { .. }
        )]
    ));

    let audio_identity = catalog_identity();
    let audio_catalog = ComponentVariantCatalog::new(
        audio_identity.clone(),
        ComponentVariantCatalogLimit::new(2).unwrap(),
        ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![audio_variant(&audio_identity, "audio-only")],
        },
    )
    .unwrap();
    assert!(matches!(
        collect_selections(&audio_catalog).as_slice(),
        [(
            WebMediaMode::AudioOnly,
            ComponentVariantSelection::AudioOnly { .. }
        )]
    ));
}

#[test]
fn parent_probe_budget_keeps_active_and_best_ranked_siblings() {
    let active = WebMediaSelectionTarget::Fixture(99);
    let choices = vec![
        fixture_choice(4, 4, WebMediaMode::VideoOnly),
        fixture_choice(99, 99, WebMediaMode::VideoAndAudio),
        fixture_choice(2, 2, WebMediaMode::AudioOnly),
        fixture_choice(1, 1, WebMediaMode::VideoAndAudio),
        fixture_choice(3, 3, WebMediaMode::VideoOnly),
    ];

    let bounded = limit_parent_choices(choices, &active, 3).unwrap();

    assert_eq!(bounded.unprobed_siblings, 2);
    assert_eq!(
        bounded
            .choices
            .iter()
            .map(|choice| choice.target.clone())
            .collect::<Vec<_>>(),
        vec![
            WebMediaSelectionTarget::Fixture(1),
            WebMediaSelectionTarget::Fixture(2),
            active,
        ]
    );
}

#[test]
fn parent_probe_budget_is_source_order_independent() {
    let active = WebMediaSelectionTarget::Fixture(4);
    let forward = vec![
        fixture_choice(1, 1, WebMediaMode::VideoAndAudio),
        fixture_choice(2, 2, WebMediaMode::VideoOnly),
        fixture_choice(3, 3, WebMediaMode::AudioOnly),
        fixture_choice(4, 4, WebMediaMode::VideoAndAudio),
    ];
    let mut reversed = forward.clone();
    reversed.reverse();

    let forward = limit_parent_choices(forward, &active, 3).unwrap();
    let reversed = limit_parent_choices(reversed, &active, 3).unwrap();

    assert_eq!(forward.choices, reversed.choices);
    assert_eq!(forward.unprobed_siblings, reversed.unprobed_siblings);
}

#[test]
fn parent_probe_budget_rejects_missing_active_choice() {
    let error = limit_parent_choices(
        vec![fixture_choice(1, 1, WebMediaMode::VideoAndAudio)],
        &WebMediaSelectionTarget::Fixture(2),
        1,
    )
    .err()
    .expect("missing active choice должен fail closed");

    assert!(error.to_string().contains("active Installed choice"));
}
