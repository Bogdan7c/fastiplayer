//! Semantic-only request и fresh-catalog rematch proofs.

use super::test_support::*;
use super::*;

/// Создаёт selection из первых exact rows catalog.
fn select_first_video_and_audio(catalog: &ComponentVariantCatalog) -> ComponentVariantSelection {
    let videos = catalog
        .required_video_variants()
        .expect("test catalog должен содержать video axis");
    let audios = catalog
        .required_audio_variants()
        .expect("test catalog должен содержать audio axis");
    catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: videos[0].exact_identity().clone(),
            audio: audios[0].exact_identity().clone(),
        })
        .expect("test exact selection должна быть valid")
}

/// Создаёт fresh VideoAndAudio catalog с теми же semantic keys.
fn fresh_video_and_audio_catalog() -> ComponentVariantCatalog {
    let fresh_identity = catalog_identity(
        parent_at_generation(1, 19, "fresh-parent-exact", "parent-semantic"),
        23,
    );
    ComponentVariantCatalog::new(
        fresh_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![
                video_variant(
                    &fresh_identity,
                    "fresh-video-720",
                    "video-semantic-720",
                    Some(720),
                ),
                video_variant(
                    &fresh_identity,
                    "fresh-video-1080",
                    "video-semantic-1080",
                    Some(1080),
                ),
            ],
            audio: vec![
                audio_variant(&fresh_identity, "fresh-audio-a", "audio-semantic-a", 1),
                audio_variant(&fresh_identity, "fresh-audio-b", "audio-semantic-b", 2),
            ],
        },
    )
    .expect("fresh test catalog должен быть valid")
}

#[test]
fn installed_selection_produces_semantic_only_layout_shaped_request() {
    let catalog = video_and_audio_catalog();
    let selection = select_first_video_and_audio(&catalog);

    match (selection.semantic_rematch_request(), selection) {
        (
            ComponentVariantSemanticSelectionRequest::VideoAndAudio { video, audio },
            ComponentVariantSelection::VideoAndAudio {
                video: selected_video,
                audio: selected_audio,
            },
        ) => {
            assert_eq!(&video, selected_video.semantic_identity());
            assert_eq!(&audio, selected_audio.semantic_identity());
            assert_eq!(video.parent(), catalog.identity().parent().semantic());
            assert_eq!(audio.parent(), catalog.identity().parent().semantic());
        }
        _ => panic!("semantic request должна сохранить VideoAndAudio shape"),
    }
}

#[test]
fn fresh_parent_and_catalog_generations_produce_fresh_exact_selection() {
    let old_catalog = video_and_audio_catalog();
    let old_selection = select_first_video_and_audio(&old_catalog);
    let semantic_request = old_selection.semantic_rematch_request();
    let fresh_catalog = fresh_video_and_audio_catalog();

    let fresh_selection = fresh_catalog
        .rematch_semantic(semantic_request)
        .expect("semantic identities должны rematch-иться в fresh catalog");

    assert_ne!(old_catalog.identity(), fresh_catalog.identity());
    assert_ne!(
        old_catalog.identity().parent().exact().generation(),
        fresh_catalog.identity().parent().exact().generation()
    );
    assert_ne!(
        old_catalog.identity().generation(),
        fresh_catalog.identity().generation()
    );
    assert_eq!(
        old_catalog.identity().parent().semantic(),
        fresh_catalog.identity().parent().semantic()
    );
    match fresh_selection {
        ComponentVariantSelection::VideoAndAudio { video, audio } => {
            assert_eq!(video.exact_identity().catalog(), fresh_catalog.identity());
            assert_eq!(audio.exact_identity().catalog(), fresh_catalog.identity());
            assert_eq!(
                video.exact_identity(),
                fresh_catalog.required_video_variants().unwrap()[0].exact_identity()
            );
            assert_eq!(
                audio.exact_identity(),
                fresh_catalog.required_audio_variants().unwrap()[0].exact_identity()
            );
        }
        _ => panic!("fresh selection должна сохранить VideoAndAudio shape"),
    }
}

#[test]
fn video_and_audio_semantics_rematch_independently_without_pair_inventory() {
    let old_catalog = video_and_audio_catalog();
    let old_videos = old_catalog.required_video_variants().unwrap();
    let old_audios = old_catalog.required_audio_variants().unwrap();
    let old_selection = old_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: old_videos[1].exact_identity().clone(),
            audio: old_audios[0].exact_identity().clone(),
        })
        .expect("old selection должна быть valid");
    let fresh_catalog = fresh_video_and_audio_catalog();

    let rematched = fresh_catalog
        .rematch_semantic(old_selection.semantic_rematch_request())
        .expect("оба независимых axis должны rematch-иться");

    assert_eq!(fresh_catalog.stored_variant_count(), 4);
    match rematched {
        ComponentVariantSelection::VideoAndAudio { video, audio } => {
            assert_eq!(
                video.exact_identity(),
                fresh_catalog.required_video_variants().unwrap()[1].exact_identity()
            );
            assert_eq!(
                audio.exact_identity(),
                fresh_catalog.required_audio_variants().unwrap()[0].exact_identity()
            );
        }
        _ => panic!("rematched selection должна сохранить VideoAndAudio shape"),
    }
}

#[test]
fn missing_video_and_audio_semantics_remain_component_typed() {
    let catalog = video_and_audio_catalog();
    let parent_semantic = catalog.identity().parent().semantic().clone();
    let video = catalog.required_video_variants().unwrap()[0]
        .semantic_identity()
        .clone();
    let audio = catalog.required_audio_variants().unwrap()[0]
        .semantic_identity()
        .clone();
    let missing_video = ComponentVariantSemanticIdentity::new(
        parent_semantic.clone(),
        ComponentKind::Video,
        ComponentVariantSemanticKey::new("missing-video").expect("key должен быть valid"),
    );
    let missing_audio = ComponentVariantSemanticIdentity::new(
        parent_semantic,
        ComponentKind::Audio,
        ComponentVariantSemanticKey::new("missing-audio").expect("key должен быть valid"),
    );

    assert_eq!(
        catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoAndAudio {
            video: missing_video,
            audio: audio.clone(),
        }),
        Err(ComponentVariantError::MissingSemanticVariant {
            component: ComponentKind::Video,
        })
    );
    assert_eq!(
        catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoAndAudio {
            video,
            audio: missing_audio,
        }),
        Err(ComponentVariantError::MissingSemanticVariant {
            component: ComponentKind::Audio,
        })
    );
}

#[test]
fn semantic_rematch_rejects_cross_source_parent_axis_and_layout() {
    let catalog = video_and_audio_catalog();
    let valid_audio = catalog.required_audio_variants().unwrap()[0]
        .semantic_identity()
        .clone();
    let other_source = parent(2, "other-source-parent", "parent-semantic");
    let other_parent = parent(1, "other-parent", "other-parent-semantic");
    let semantic_key =
        ComponentVariantSemanticKey::new("video-semantic-720").expect("key должен быть valid");

    for (video, expected_error) in [
        (
            ComponentVariantSemanticIdentity::new(
                other_source.semantic().clone(),
                ComponentKind::Video,
                semantic_key.clone(),
            ),
            ComponentVariantError::SourceMismatch,
        ),
        (
            ComponentVariantSemanticIdentity::new(
                other_parent.semantic().clone(),
                ComponentKind::Video,
                semantic_key.clone(),
            ),
            ComponentVariantError::CrossParent,
        ),
        (
            ComponentVariantSemanticIdentity::new(
                catalog.identity().parent().semantic().clone(),
                ComponentKind::Audio,
                semantic_key,
            ),
            ComponentVariantError::WrongAxis {
                expected: ComponentKind::Video,
                provided: ComponentKind::Audio,
            },
        ),
    ] {
        assert_eq!(
            catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoAndAudio {
                video,
                audio: valid_audio.clone(),
            }),
            Err(expected_error)
        );
    }

    let valid_video = catalog.required_video_variants().unwrap()[0]
        .semantic_identity()
        .clone();
    assert_eq!(
        catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoOnly {
            video: valid_video,
        }),
        Err(ComponentVariantError::LayoutMismatch)
    );
}

#[test]
fn video_only_and_audio_only_rematch_to_fresh_exact_rows() {
    let old_video_identity = catalog_identity(parent(1, "old-v", "stable-v-parent"), 1);
    let old_video_catalog = ComponentVariantCatalog::new(
        old_video_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(
                &old_video_identity,
                "old-video",
                "stable-video",
                Some(720),
            )],
        },
    )
    .unwrap();
    let old_video_selection = old_video_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: old_video_catalog.required_video_variants().unwrap()[0]
                .exact_identity()
                .clone(),
        })
        .unwrap();
    let fresh_video_identity =
        catalog_identity(parent_at_generation(1, 9, "fresh-v", "stable-v-parent"), 10);
    let fresh_video_catalog = ComponentVariantCatalog::new(
        fresh_video_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(
                &fresh_video_identity,
                "fresh-video",
                "stable-video",
                Some(720),
            )],
        },
    )
    .unwrap();
    assert!(matches!(
        fresh_video_catalog
            .rematch_semantic(old_video_selection.semantic_rematch_request())
            .unwrap(),
        ComponentVariantSelection::VideoOnly { video }
            if video.exact_identity().catalog() == fresh_video_catalog.identity()
    ));

    let old_audio_identity = catalog_identity(parent(2, "old-a", "stable-a-parent"), 2);
    let old_audio_catalog = ComponentVariantCatalog::new(
        old_audio_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![audio_variant(
                &old_audio_identity,
                "old-audio",
                "stable-audio",
                1,
            )],
        },
    )
    .unwrap();
    let old_audio_selection = old_audio_catalog
        .select_exact(ComponentVariantSelectionRequest::AudioOnly {
            audio: old_audio_catalog.required_audio_variants().unwrap()[0]
                .exact_identity()
                .clone(),
        })
        .unwrap();
    let fresh_audio_identity = catalog_identity(
        parent_at_generation(2, 11, "fresh-a", "stable-a-parent"),
        12,
    );
    let fresh_audio_catalog = ComponentVariantCatalog::new(
        fresh_audio_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![audio_variant(
                &fresh_audio_identity,
                "fresh-audio",
                "stable-audio",
                1,
            )],
        },
    )
    .unwrap();
    assert!(matches!(
        fresh_audio_catalog
            .rematch_semantic(old_audio_selection.semantic_rematch_request())
            .unwrap(),
        ComponentVariantSelection::AudioOnly { audio }
            if audio.exact_identity().catalog() == fresh_audio_catalog.identity()
    ));
}

#[test]
fn failed_semantic_rematch_does_not_mutate_old_selection() {
    let old_catalog = video_and_audio_catalog();
    let old_selection = select_first_video_and_audio(&old_catalog);
    let unchanged_selection = old_selection.clone();
    let fresh_catalog = fresh_video_and_audio_catalog();
    let mut request = old_selection.semantic_rematch_request();
    if let ComponentVariantSemanticSelectionRequest::VideoAndAudio { video, .. } = &mut request {
        *video = ComponentVariantSemanticIdentity::new(
            video.parent().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new("missing").expect("key должен быть valid"),
        );
    }

    assert_eq!(
        fresh_catalog.rematch_semantic(request),
        Err(ComponentVariantError::MissingSemanticVariant {
            component: ComponentKind::Video,
        })
    );
    assert_eq!(old_selection, unchanged_selection);
}
