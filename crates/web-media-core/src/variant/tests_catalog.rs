//! Catalog admission, selection, replacement и ranking proofs.

use super::test_support::*;
use super::*;
use crate::{PreferredVideoHeight, VideoHeight};

#[test]
fn checked_limit_rejects_zero_above_ceiling_and_catalog_overflow() {
    assert_eq!(
        ComponentVariantCatalogLimit::new(0),
        Err(ComponentVariantCatalogLimitError::Zero)
    );
    assert_eq!(
        ComponentVariantCatalogLimit::new(MAX_COMPONENT_VARIANT_CATALOG_ENTRIES + 1),
        Err(ComponentVariantCatalogLimitError::AboveMaximum {
            provided_entries: MAX_COMPONENT_VARIANT_CATALOG_ENTRIES + 1,
            maximum_entries: MAX_COMPONENT_VARIANT_CATALOG_ENTRIES,
        })
    );

    let identity = catalog_identity(parent(1, "parent", "semantic"), 1);
    assert_eq!(
        ComponentVariantCatalog::new(
            identity.clone(),
            ComponentVariantCatalogLimit::new(1).expect("limit должен быть valid"),
            ComponentVariantCatalogEntries::VideoAndAudio {
                video: vec![video_variant(&identity, "v", "sv", Some(720))],
                audio: vec![audio_variant(&identity, "a", "sa", 1)],
            },
        ),
        Err(ComponentVariantError::CatalogLimitExceeded {
            provided_entries: 2,
            maximum_entries: 1,
        })
    );
}

#[test]
fn catalog_rejects_empty_required_axis() {
    let identity = catalog_identity(parent(1, "parent", "semantic"), 1);
    assert_eq!(
        ComponentVariantCatalog::new(
            identity.clone(),
            generous_limit(),
            ComponentVariantCatalogEntries::VideoAndAudio {
                video: Vec::new(),
                audio: vec![audio_variant(&identity, "a", "sa", 1)],
            },
        ),
        Err(ComponentVariantError::MissingRequiredAxis {
            component: ComponentKind::Video,
        })
    );
}

#[test]
fn catalog_rejects_cross_source_cross_parent_stale_generation_and_wrong_axis() {
    let identity = catalog_identity(parent(1, "parent", "semantic"), 5);
    let other_source = catalog_identity(parent(2, "parent", "semantic"), 5);
    let other_parent = catalog_identity(parent(1, "other-parent", "other-semantic"), 5);
    let stale = catalog_identity(identity.parent().clone(), 4);

    for (variant_identity, expected_error) in [
        (other_source, ComponentVariantError::SourceMismatch),
        (other_parent, ComponentVariantError::CrossParent),
        (
            stale,
            ComponentVariantError::StaleCatalogGeneration {
                expected: ComponentVariantCatalogGeneration::new(5),
                provided: ComponentVariantCatalogGeneration::new(4),
            },
        ),
    ] {
        let variant = video_variant(&variant_identity, "v", "sv", Some(720));
        assert_eq!(
            ComponentVariantCatalog::new(
                identity.clone(),
                generous_limit(),
                ComponentVariantCatalogEntries::VideoOnly {
                    video: vec![variant],
                },
            ),
            Err(expected_error)
        );
    }

    let wrong_axis = VideoComponentVariant::new(
        ComponentVariantExactIdentity::new(
            identity.clone(),
            ComponentKind::Audio,
            ComponentVariantExactKey::new("v").expect("key должен быть valid"),
        ),
        ComponentVariantSemanticIdentity::new(
            identity.parent().semantic().clone(),
            ComponentKind::Audio,
            ComponentVariantSemanticKey::new("sv").expect("key должен быть valid"),
        ),
        video_track(Some(720)),
    );
    assert_eq!(
        ComponentVariantCatalog::new(
            identity,
            generous_limit(),
            ComponentVariantCatalogEntries::VideoOnly {
                video: vec![wrong_axis],
            },
        ),
        Err(ComponentVariantError::WrongAxis {
            expected: ComponentKind::Video,
            provided: ComponentKind::Audio,
        })
    );
}

#[test]
fn catalog_rejects_semantic_source_parent_and_axis_mismatch() {
    let identity = catalog_identity(parent(1, "parent", "stable-parent"), 5);
    let other_source_parent = parent(2, "parent", "stable-parent");
    let other_semantic_parent = parent(1, "other-parent", "other-semantic-parent");

    let invalid_semantic_identities = [
        (
            ComponentVariantSemanticIdentity::new(
                other_source_parent.semantic().clone(),
                ComponentKind::Video,
                ComponentVariantSemanticKey::new("stable-variant")
                    .expect("semantic key должен быть valid"),
            ),
            ComponentVariantError::SourceMismatch,
        ),
        (
            ComponentVariantSemanticIdentity::new(
                other_semantic_parent.semantic().clone(),
                ComponentKind::Video,
                ComponentVariantSemanticKey::new("stable-variant")
                    .expect("semantic key должен быть valid"),
            ),
            ComponentVariantError::CrossParent,
        ),
        (
            ComponentVariantSemanticIdentity::new(
                identity.parent().semantic().clone(),
                ComponentKind::Audio,
                ComponentVariantSemanticKey::new("stable-variant")
                    .expect("semantic key должен быть valid"),
            ),
            ComponentVariantError::WrongAxis {
                expected: ComponentKind::Video,
                provided: ComponentKind::Audio,
            },
        ),
    ];

    for (semantic_identity, expected_error) in invalid_semantic_identities {
        let exact_identity = ComponentVariantExactIdentity::new(
            identity.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new("exact").expect("exact key должен быть valid"),
        );
        let variant =
            VideoComponentVariant::new(exact_identity, semantic_identity, video_track(Some(720)));
        assert_eq!(
            ComponentVariantCatalog::new(
                identity.clone(),
                generous_limit(),
                ComponentVariantCatalogEntries::VideoOnly {
                    video: vec![variant],
                },
            ),
            Err(expected_error)
        );
    }
}

#[test]
fn catalog_rejects_duplicate_exact_and_ambiguous_semantic_identities() {
    let identity = catalog_identity(parent(1, "parent", "semantic"), 1);
    let duplicate_exact = vec![
        video_variant(&identity, "same", "semantic-a", Some(720)),
        video_variant(&identity, "same", "semantic-b", Some(1080)),
    ];
    assert_eq!(
        ComponentVariantCatalog::new(
            identity.clone(),
            generous_limit(),
            ComponentVariantCatalogEntries::VideoOnly {
                video: duplicate_exact,
            },
        ),
        Err(ComponentVariantError::DuplicateExactIdentity {
            component: ComponentKind::Video,
        })
    );

    let ambiguous_semantic = vec![
        audio_variant(&identity, "audio-a", "same-semantic", 1),
        audio_variant(&identity, "audio-b", "same-semantic", 2),
    ];
    assert_eq!(
        ComponentVariantCatalog::new(
            identity,
            generous_limit(),
            ComponentVariantCatalogEntries::AudioOnly {
                audio: ambiguous_semantic,
            },
        ),
        Err(ComponentVariantError::AmbiguousSemanticIdentity {
            component: ComponentKind::Audio,
        })
    );
}

#[test]
fn exact_selection_validates_layout_and_missing_variant() {
    let catalog = video_and_audio_catalog();
    let video = catalog
        .required_video_variants()
        .expect("video axis должна существовать");
    let audio = catalog
        .required_audio_variants()
        .expect("audio axis должна существовать");
    let selection = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: video[0].exact_identity().clone(),
            audio: audio[1].exact_identity().clone(),
        })
        .expect("exact selection должна быть valid");
    assert!(matches!(
        selection,
        ComponentVariantSelection::VideoAndAudio { .. }
    ));

    assert_eq!(
        catalog.select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video[0].exact_identity().clone(),
        }),
        Err(ComponentVariantError::LayoutMismatch)
    );

    let missing = ComponentVariantExactIdentity::new(
        catalog.identity().clone(),
        ComponentKind::Video,
        ComponentVariantExactKey::new("missing").expect("key должен быть valid"),
    );
    assert_eq!(
        catalog.select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: missing,
            audio: audio[0].exact_identity().clone(),
        }),
        Err(ComponentVariantError::MissingVariant {
            component: ComponentKind::Video,
        })
    );
}

#[test]
fn exact_selection_request_round_trips_all_layouts_and_rejects_wrong_catalog() {
    let video_and_audio_catalog = video_and_audio_catalog();
    let video = video_and_audio_catalog
        .required_video_variants()
        .expect("video axis должна существовать");
    let audio = video_and_audio_catalog
        .required_audio_variants()
        .expect("audio axis должна существовать");
    let video_and_audio_selection = video_and_audio_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: video[1].exact_identity().clone(),
            audio: audio[0].exact_identity().clone(),
        })
        .expect("VideoAndAudio selection должна быть valid");
    assert_eq!(
        video_and_audio_catalog.select_exact(video_and_audio_selection.exact_selection_request()),
        Ok(video_and_audio_selection.clone())
    );

    let wrong_identity = catalog_identity(parent(1, "parent", "parent-semantic"), 4);
    let wrong_catalog = ComponentVariantCatalog::new(
        wrong_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![video_variant(
                &wrong_identity,
                "video-1080",
                "video-semantic-1080",
                Some(1080),
            )],
            audio: vec![audio_variant(
                &wrong_identity,
                "audio-a",
                "audio-semantic-a",
                1,
            )],
        },
    )
    .expect("wrong-generation catalog должен быть structurally valid");
    assert_eq!(
        wrong_catalog.select_exact(video_and_audio_selection.exact_selection_request()),
        Err(ComponentVariantError::StaleCatalogGeneration {
            expected: ComponentVariantCatalogGeneration::new(4),
            provided: ComponentVariantCatalogGeneration::new(3),
        })
    );

    let video_identity = catalog_identity(parent(2, "video-parent", "video-semantic"), 1);
    let video_catalog = ComponentVariantCatalog::new(
        video_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(
                &video_identity,
                "video",
                "video-semantic",
                Some(720),
            )],
        },
    )
    .expect("VideoOnly catalog должен быть valid");
    let video_selection = video_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video_catalog
                .required_video_variants()
                .expect("video axis должна существовать")[0]
                .exact_identity()
                .clone(),
        })
        .expect("VideoOnly selection должна быть valid");
    assert_eq!(
        video_catalog.select_exact(video_selection.exact_selection_request()),
        Ok(video_selection)
    );

    let audio_identity = catalog_identity(parent(3, "audio-parent", "audio-semantic"), 1);
    let audio_catalog = ComponentVariantCatalog::new(
        audio_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![audio_variant(&audio_identity, "audio", "audio-semantic", 1)],
        },
    )
    .expect("AudioOnly catalog должен быть valid");
    let audio_selection = audio_catalog
        .select_exact(ComponentVariantSelectionRequest::AudioOnly {
            audio: audio_catalog
                .required_audio_variants()
                .expect("audio axis должна существовать")[0]
                .exact_identity()
                .clone(),
        })
        .expect("AudioOnly selection должна быть valid");
    assert_eq!(
        audio_catalog.select_exact(audio_selection.exact_selection_request()),
        Ok(audio_selection)
    );
}

#[test]
fn replacements_preserve_the_other_axis_byte_for_byte() {
    let catalog = video_and_audio_catalog();
    let videos = catalog
        .required_video_variants()
        .expect("video axis должна существовать");
    let audios = catalog
        .required_audio_variants()
        .expect("audio axis должна существовать");
    let original = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: videos[0].exact_identity().clone(),
            audio: audios[0].exact_identity().clone(),
        })
        .expect("selection должна быть valid");

    let changed_video = original
        .replace_video(&catalog, videos[1].exact_identity())
        .expect("video replacement должна быть valid");
    match (&original, &changed_video) {
        (
            ComponentVariantSelection::VideoAndAudio {
                audio: original_audio,
                ..
            },
            ComponentVariantSelection::VideoAndAudio {
                audio: preserved_audio,
                ..
            },
        ) => assert_eq!(preserved_audio, original_audio),
        _ => panic!("test selection shape должна остаться VideoAndAudio"),
    }

    let changed_audio = original
        .replace_audio(&catalog, audios[1].exact_identity())
        .expect("audio replacement должна быть valid");
    match (&original, &changed_audio) {
        (
            ComponentVariantSelection::VideoAndAudio {
                video: original_video,
                ..
            },
            ComponentVariantSelection::VideoAndAudio {
                video: preserved_video,
                ..
            },
        ) => assert_eq!(preserved_video, original_video),
        _ => panic!("test selection shape должна остаться VideoAndAudio"),
    }
}

#[test]
fn failed_replacement_leaves_original_selection_unchanged() {
    let catalog = video_and_audio_catalog();
    let video = catalog
        .required_video_variants()
        .expect("video axis должна существовать");
    let audio = catalog
        .required_audio_variants()
        .expect("audio axis должна существовать");
    let original = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: video[0].exact_identity().clone(),
            audio: audio[0].exact_identity().clone(),
        })
        .expect("selection должна быть valid");
    let before_failure = original.clone();
    let missing = ComponentVariantExactIdentity::new(
        catalog.identity().clone(),
        ComponentKind::Video,
        ComponentVariantExactKey::new("missing").expect("key должен быть valid"),
    );

    assert_eq!(
        original.replace_video(&catalog, &missing),
        Err(ComponentVariantError::MissingVariant {
            component: ComponentKind::Video,
        })
    );
    assert_eq!(original, before_failure);
}

#[test]
fn video_only_and_audio_only_have_exact_non_optional_shapes() {
    let video_identity = catalog_identity(parent(1, "video-parent", "video-semantic"), 1);
    let video_catalog = ComponentVariantCatalog::new(
        video_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(
                &video_identity,
                "video",
                "video-semantic",
                Some(720),
            )],
        },
    )
    .expect("VideoOnly catalog должен быть valid");
    let selected_video = video_catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video_catalog
                .required_video_variants()
                .expect("video axis должна существовать")[0]
                .exact_identity()
                .clone(),
        })
        .expect("VideoOnly selection должна быть valid");
    assert!(matches!(
        selected_video,
        ComponentVariantSelection::VideoOnly { .. }
    ));

    let audio_identity = catalog_identity(parent(2, "audio-parent", "audio-semantic"), 1);
    let audio_catalog = ComponentVariantCatalog::new(
        audio_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![audio_variant(&audio_identity, "audio", "audio-semantic", 1)],
        },
    )
    .expect("AudioOnly catalog должен быть valid");
    let selected_audio = audio_catalog
        .select_exact(ComponentVariantSelectionRequest::AudioOnly {
            audio: audio_catalog
                .required_audio_variants()
                .expect("audio axis должна существовать")[0]
                .exact_identity()
                .clone(),
        })
        .expect("AudioOnly selection должна быть valid");
    assert!(matches!(
        selected_audio,
        ComponentVariantSelection::AudioOnly { .. }
    ));
}

#[test]
fn video_and_audio_storage_is_additive_not_cartesian() {
    let identity = catalog_identity(parent(1, "parent", "semantic"), 1);
    let catalog = ComponentVariantCatalog::new(
        identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![
                video_variant(&identity, "video-a", "video-semantic-a", Some(480)),
                video_variant(&identity, "video-b", "video-semantic-b", Some(720)),
                video_variant(&identity, "video-c", "video-semantic-c", Some(1080)),
            ],
            audio: vec![
                audio_variant(&identity, "audio-a", "audio-semantic-a", 1),
                audio_variant(&identity, "audio-b", "audio-semantic-b", 2),
            ],
        },
    )
    .expect("cardinality test catalog должен быть valid");
    let video_count = catalog.required_video_variants().unwrap().len();
    let audio_count = catalog.required_audio_variants().unwrap().len();
    assert_eq!(video_count, 3);
    assert_eq!(audio_count, 2);
    assert_eq!(catalog.stored_variant_count(), video_count + audio_count);
    assert_ne!(catalog.stored_variant_count(), video_count * audio_count);
}

#[test]
fn preferred_height_uses_existing_rank_and_preserves_first_on_equality() {
    let identity = catalog_identity(parent(1, "parent", "semantic"), 1);
    let catalog = ComponentVariantCatalog::new(
        identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![
                video_variant(&identity, "exact-first", "s-exact-first", Some(720)),
                video_variant(&identity, "exact-second", "s-exact-second", Some(720)),
                video_variant(&identity, "lower", "s-lower", Some(480)),
                video_variant(&identity, "higher", "s-higher", Some(1080)),
                video_variant(&identity, "missing", "s-missing", None),
            ],
        },
    )
    .expect("height test catalog должен быть valid");

    let prefer_720 = PreferredHeightPolicy::Prefer(
        PreferredVideoHeight::new(720).expect("preferred height должна быть valid"),
    );
    assert_eq!(
        catalog
            .preferred_video_variant(prefer_720)
            .expect("video variant должна существовать")
            .exact_identity(),
        catalog.required_video_variants().unwrap()[0].exact_identity()
    );

    let prefer_800 = PreferredHeightPolicy::Prefer(
        PreferredVideoHeight::new(800).expect("preferred height должна быть valid"),
    );
    assert_eq!(
        catalog
            .preferred_video_variant(prefer_800)
            .expect("lower fallback должна существовать")
            .track()
            .height(),
        Some(VideoHeight::new(720).unwrap())
    );

    let higher_only_identity = catalog_identity(parent(2, "higher", "higher-semantic"), 1);
    let higher_only = ComponentVariantCatalog::new(
        higher_only_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![
                video_variant(&higher_only_identity, "higher", "s-higher", Some(1080)),
                video_variant(&higher_only_identity, "missing", "s-missing", None),
            ],
        },
    )
    .expect("higher fallback catalog должен быть valid");
    assert_eq!(
        higher_only
            .preferred_video_variant(prefer_800)
            .expect("higher fallback должна существовать")
            .track()
            .height(),
        Some(VideoHeight::new(1080).unwrap())
    );

    let missing_identity = catalog_identity(parent(3, "missing", "missing-semantic"), 1);
    let missing_only = ComponentVariantCatalog::new(
        missing_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![video_variant(
                &missing_identity,
                "missing",
                "s-missing",
                None,
            )],
        },
    )
    .expect("missing height catalog должен быть valid");
    assert_eq!(
        missing_only
            .preferred_video_variant(prefer_800)
            .expect("missing fallback должна существовать")
            .track()
            .height(),
        None
    );
}
