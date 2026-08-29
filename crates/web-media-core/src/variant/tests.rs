//! Public catalog boundary tests, kept outside the instrumented production report.

use super::test_support::*;
use super::*;
use crate::PreferredVideoHeight;

#[test]
fn single_axis_catalogs_preserve_layout_without_inventing_a_missing_axis() {
    let video_identity = catalog_identity(parent(41, "video-parent", "video-parent-semantic"), 3);
    let videos = vec![
        video_variant(&video_identity, "video-1080", "semantic-1080", Some(1080)),
        video_variant(&video_identity, "video-720", "semantic-720", Some(720)),
    ];
    let selected_video_exact = videos[1].exact_identity().clone();
    let selected_video_semantic = videos[1].semantic_identity().clone();
    let unrelated_audio = audio_variant(&video_identity, "audio-probe", "audio-probe", 2);
    let video_catalog = ComponentVariantCatalog::new(
        video_identity,
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly { video: videos },
    )
    .expect("VideoOnly catalog должен пройти admission");

    assert_eq!(video_catalog.stored_variant_count(), 2);
    assert!(video_catalog.compatibility().is_none());
    assert!(video_catalog.coupled_presentations().is_empty());
    assert!(video_catalog.is_video_only_selectable(&selected_video_exact));
    assert!(!video_catalog.is_audio_only_selectable(unrelated_audio.exact_identity()));
    let missing_audio = video_catalog
        .required_audio_variants()
        .expect_err("VideoOnly catalog не должен придумывать audio axis");
    assert_eq!(
        missing_audio.to_string(),
        "required Audio axis пуста или отсутствует"
    );
    assert_eq!(
        video_catalog
            .preferred_video_variant(PreferredHeightPolicy::Prefer(
                PreferredVideoHeight::new(720).expect("preferred height должна быть valid"),
            ))
            .expect("preferred video должна существовать")
            .exact_identity(),
        &selected_video_exact
    );
    assert!(matches!(
        video_catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: selected_video_exact,
            })
            .expect("exact VideoOnly selection должна быть valid"),
        ComponentVariantSelection::VideoOnly { .. }
    ));
    assert!(matches!(
        video_catalog
            .rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoOnly {
                video: selected_video_semantic,
            })
            .expect("semantic VideoOnly rematch должен быть valid"),
        ComponentVariantSelection::VideoOnly { .. }
    ));

    let audio_identity = catalog_identity(parent(42, "audio-parent", "audio-parent-semantic"), 4);
    let audio = audio_variant(&audio_identity, "audio-main", "audio-semantic", 1);
    let audio_exact = audio.exact_identity().clone();
    let audio_semantic = audio.semantic_identity().clone();
    let unrelated_video = video_variant(&audio_identity, "video-probe", "video-probe", Some(360));
    let audio_catalog = ComponentVariantCatalog::new(
        audio_identity,
        generous_limit(),
        ComponentVariantCatalogEntries::AudioOnly { audio: vec![audio] },
    )
    .expect("AudioOnly catalog должен пройти admission");

    assert_eq!(audio_catalog.stored_variant_count(), 1);
    assert!(audio_catalog.compatibility().is_none());
    assert!(audio_catalog.coupled_presentations().is_empty());
    assert!(audio_catalog.is_audio_only_selectable(&audio_exact));
    assert!(!audio_catalog.is_video_only_selectable(unrelated_video.exact_identity()));
    let missing_video = audio_catalog
        .required_video_variants()
        .expect_err("AudioOnly catalog не должен придумывать video axis");
    assert_eq!(
        missing_video.to_string(),
        "required Video axis пуста или отсутствует"
    );
    assert!(matches!(
        audio_catalog
            .select_exact(ComponentVariantSelectionRequest::AudioOnly { audio: audio_exact })
            .expect("exact AudioOnly selection должна быть valid"),
        ComponentVariantSelection::AudioOnly { .. }
    ));
    assert!(matches!(
        audio_catalog
            .rematch_semantic(ComponentVariantSemanticSelectionRequest::AudioOnly {
                audio: audio_semantic,
            })
            .expect("semantic AudioOnly rematch должен быть valid"),
        ComponentVariantSelection::AudioOnly { .. }
    ));
}

#[test]
fn topology_standalone_rows_are_selectable_exactly_and_after_semantic_reopen() {
    let identity = catalog_identity(parent(43, "topology-parent", "topology-semantic"), 5);
    let videos = vec![
        video_variant(
            &identity,
            "video-allowed",
            "video-stable-allowed",
            Some(720),
        ),
        video_variant(&identity, "video-paired", "video-stable-paired", Some(1080)),
    ];
    let audios = vec![
        audio_variant(&identity, "audio-paired", "audio-stable-paired", 1),
        audio_variant(&identity, "audio-allowed", "audio-stable-allowed", 2),
    ];
    let video_allowed_exact = videos[0].exact_identity().clone();
    let video_denied_exact = videos[1].exact_identity().clone();
    let video_allowed_semantic = videos[0].semantic_identity().clone();
    let video_denied_semantic = videos[1].semantic_identity().clone();
    let audio_denied_exact = audios[0].exact_identity().clone();
    let audio_allowed_exact = audios[1].exact_identity().clone();
    let audio_denied_semantic = audios[0].semantic_identity().clone();
    let audio_allowed_semantic = audios[1].semantic_identity().clone();
    let catalog = ComponentVariantCatalog::new(
        identity,
        generous_limit(),
        ComponentVariantCatalogEntries::Topology {
            video: videos,
            audio: audios,
            compatibility: ComponentVariantCompatibilityEntries::Unavailable,
            coupled: vec![],
            video_only: vec![video_allowed_exact.clone()],
            audio_only: vec![audio_allowed_exact.clone()],
        },
    )
    .expect("standalone topology должна пройти admission");

    assert_eq!(catalog.stored_variant_count(), 4);
    assert_eq!(
        catalog
            .compatibility()
            .expect("Topology должна публиковать relation")
            .logical_edge_count(),
        0
    );
    assert!(catalog.is_video_only_selectable(&video_allowed_exact));
    assert!(!catalog.is_video_only_selectable(&video_denied_exact));
    assert!(catalog.is_audio_only_selectable(&audio_allowed_exact));
    assert!(!catalog.is_audio_only_selectable(&audio_denied_exact));

    assert!(matches!(
        catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: video_allowed_exact,
            })
            .expect("published video-only row должна выбираться"),
        ComponentVariantSelection::VideoOnly { .. }
    ));
    assert_eq!(
        catalog.select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video_denied_exact,
        }),
        Err(ComponentVariantError::IncompatibleComponentPair)
    );
    assert!(matches!(
        catalog
            .select_exact(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio_allowed_exact,
            })
            .expect("published audio-only row должна выбираться"),
        ComponentVariantSelection::AudioOnly { .. }
    ));
    assert_eq!(
        catalog.select_exact(ComponentVariantSelectionRequest::AudioOnly {
            audio: audio_denied_exact,
        }),
        Err(ComponentVariantError::IncompatibleComponentPair)
    );

    assert!(matches!(
        catalog
            .rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoOnly {
                video: video_allowed_semantic,
            })
            .expect("published semantic video row должна rematch-иться"),
        ComponentVariantSelection::VideoOnly { .. }
    ));
    assert_eq!(
        catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoOnly {
            video: video_denied_semantic,
        }),
        Err(ComponentVariantError::IncompatibleComponentPair)
    );
    assert!(matches!(
        catalog
            .rematch_semantic(ComponentVariantSemanticSelectionRequest::AudioOnly {
                audio: audio_allowed_semantic,
            })
            .expect("published semantic audio row должна rematch-иться"),
        ComponentVariantSelection::AudioOnly { .. }
    ));
    let denied = catalog
        .rematch_semantic(ComponentVariantSemanticSelectionRequest::AudioOnly {
            audio: audio_denied_semantic,
        })
        .expect_err("неопубликованная standalone row должна остаться incompatible");
    assert_eq!(
        denied.to_string(),
        "video/audio component pair не разрешена catalog relation"
    );
}

#[test]
fn sparse_topology_semantic_rematch_preserves_only_proven_component_pairs() {
    let identity = catalog_identity(parent(49, "sparse-parent", "sparse-parent-semantic"), 11);
    let videos = vec![
        video_variant(
            &identity,
            "video-allowed",
            "video-semantic-allowed",
            Some(720),
        ),
        video_variant(&identity, "video-other", "video-semantic-other", Some(1080)),
    ];
    let audios = vec![
        audio_variant(&identity, "audio-allowed", "audio-semantic-allowed", 1),
        audio_variant(&identity, "audio-other", "audio-semantic-other", 2),
    ];
    let allowed_video_exact = videos[0].exact_identity().clone();
    let allowed_audio_exact = audios[0].exact_identity().clone();
    let incompatible_video_semantic = videos[1].semantic_identity().clone();
    let incompatible_audio_semantic = audios[1].semantic_identity().clone();
    let catalog = ComponentVariantCatalog::new(
        identity,
        generous_limit(),
        ComponentVariantCatalogEntries::Topology {
            video: videos,
            audio: audios,
            compatibility: ComponentVariantCompatibilityEntries::Sparse {
                edge_limit: generous_edge_limit(),
                edges: vec![ComponentVariantCompatibilityEdge::new(
                    allowed_video_exact.clone(),
                    allowed_audio_exact.clone(),
                )],
            },
            coupled: vec![],
            video_only: vec![],
            audio_only: vec![],
        },
    )
    .expect("sparse topology с одной доказанной парой должна пройти admission");

    let installed = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: allowed_video_exact.clone(),
            audio: allowed_audio_exact.clone(),
        })
        .expect("доказанная exact pair должна устанавливаться");
    let rematched = catalog
        .rematch_semantic(installed.semantic_rematch_request())
        .expect("semantic rematch должен сохранить доказанную pair");
    let ComponentVariantSelection::VideoAndAudio { video, audio } = rematched else {
        panic!("semantic rematch не должен менять layout доказанной pair");
    };
    assert_eq!(video.exact_identity(), &allowed_video_exact);
    assert_eq!(audio.exact_identity(), &allowed_audio_exact);

    assert_eq!(
        catalog.rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoAndAudio {
            video: incompatible_video_semantic,
            audio: incompatible_audio_semantic,
        }),
        Err(ComponentVariantError::IncompatibleComponentPair),
        "существование обеих rows не доказывает их Cartesian compatibility"
    );
}

#[test]
fn catalog_admission_diagnostics_explain_empty_topology_limits_and_duplicates() {
    let empty_identity = catalog_identity(parent(44, "empty-parent", "empty-semantic"), 6);
    let empty_error = ComponentVariantCatalog::new(
        empty_identity,
        generous_limit(),
        ComponentVariantCatalogEntries::Topology {
            video: vec![],
            audio: vec![],
            compatibility: ComponentVariantCompatibilityEntries::Unavailable,
            coupled: vec![],
            video_only: vec![],
            audio_only: vec![],
        },
    )
    .expect_err("Topology без selectable presentation должна быть отклонена");
    assert_eq!(
        empty_error.to_string(),
        "catalog не содержит selectable presentation"
    );

    let limited_identity = catalog_identity(parent(45, "limited-parent", "limited-semantic"), 7);
    let limit_error = ComponentVariantCatalog::new(
        limited_identity.clone(),
        ComponentVariantCatalogLimit::new(1).expect("test limit должна быть valid"),
        ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![video_variant(
                &limited_identity,
                "limited-video",
                "limited-video-semantic",
                Some(720),
            )],
            audio: vec![audio_variant(
                &limited_identity,
                "limited-audio",
                "limited-audio-semantic",
                1,
            )],
        },
    )
    .expect_err("catalog выше caller budget должен быть отклонён");
    assert_eq!(
        limit_error.to_string(),
        "catalog содержит 2 rows при лимите 1"
    );

    let duplicate_identity =
        catalog_identity(parent(46, "duplicate-parent", "duplicate-semantic"), 8);
    let duplicate_error = ComponentVariantCatalog::new(
        duplicate_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![
                video_variant(
                    &duplicate_identity,
                    "same-exact",
                    "semantic-first",
                    Some(720),
                ),
                video_variant(
                    &duplicate_identity,
                    "same-exact",
                    "semantic-second",
                    Some(1080),
                ),
            ],
        },
    )
    .expect_err("duplicate exact identity должна быть отклонена");
    assert_eq!(
        duplicate_error.to_string(),
        "catalog содержит duplicate exact Video identity"
    );

    let ambiguous_identity =
        catalog_identity(parent(47, "ambiguous-parent", "ambiguous-semantic"), 9);
    let ambiguous_error = ComponentVariantCatalog::new(
        ambiguous_identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![
                video_variant(
                    &ambiguous_identity,
                    "exact-first",
                    "same-semantic",
                    Some(720),
                ),
                video_variant(
                    &ambiguous_identity,
                    "exact-second",
                    "same-semantic",
                    Some(1080),
                ),
            ],
        },
    )
    .expect_err("ambiguous semantic identity должна быть отклонена");
    assert_eq!(
        ambiguous_error.to_string(),
        "catalog содержит ambiguous semantic Video identity"
    );
}

#[test]
fn selection_diagnostics_distinguish_missing_exact_semantic_and_layout_failures() {
    let identity = catalog_identity(parent(48, "selection-parent", "selection-semantic"), 10);
    let video = video_variant(&identity, "known-video", "known-semantic", Some(720));
    let catalog = ComponentVariantCatalog::new(
        identity.clone(),
        generous_limit(),
        ComponentVariantCatalogEntries::VideoOnly { video: vec![video] },
    )
    .expect("selection diagnostic catalog должен быть valid");

    let missing_exact = ComponentVariantExactIdentity::new(
        identity.clone(),
        ComponentKind::Video,
        ComponentVariantExactKey::new("missing-exact").expect("exact key должна быть valid"),
    );
    let exact_error = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: missing_exact,
        })
        .expect_err("missing exact row должна быть typed failure");
    assert_eq!(
        exact_error.to_string(),
        "exact Video variant отсутствует в catalog"
    );

    let missing_semantic = ComponentVariantSemanticIdentity::new(
        identity.parent().semantic().clone(),
        ComponentKind::Video,
        ComponentVariantSemanticKey::new("missing-semantic")
            .expect("semantic key должна быть valid"),
    );
    let semantic_error = catalog
        .rematch_semantic(ComponentVariantSemanticSelectionRequest::VideoOnly {
            video: missing_semantic,
        })
        .expect_err("missing semantic row должна быть typed failure");
    assert_eq!(
        semantic_error.to_string(),
        "semantic Video variant отсутствует в catalog"
    );

    let audio = audio_variant(&identity, "wrong-layout-audio", "wrong-layout-audio", 2);
    let layout_error = catalog
        .select_exact(ComponentVariantSelectionRequest::AudioOnly {
            audio: audio.exact_identity().clone(),
        })
        .expect_err("wrong selection shape должна быть typed failure");
    assert_eq!(
        layout_error.to_string(),
        "component selection shape не совпадает с catalog layout"
    );
}
