use std::sync::Arc;

use web_media_core::{
    AudioComponentVariant, AudioTrackDescriptor, Bitrate, CandidateFormatIdentity,
    CandidateIdentity, ChannelCount, ComponentKind, ComponentVariantCatalog,
    ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEntries, ComponentVariantError, ComponentVariantExactIdentity,
    ComponentVariantExactKey, ComponentVariantSelection, ComponentVariantSelectionRequest,
    ComponentVariantSemanticIdentity, ComponentVariantSemanticKey,
    ComponentVariantSemanticSelectionRequest, CoupledComponentVariant, CoupledVariantExactIdentity,
    CoupledVariantSemanticIdentity, DynamicRange, ExactSelectionIdentity, ExtractionGeneration,
    FrameRate, LanguageTag, NormalizedCodec, RawCodecIdentity, SampleRate, SemanticIdentity,
    SourceIdentity, VideoComponentVariant, VideoHeight, VideoTrackDescriptor, VideoWidth,
};

use super::component_variants::*;
use super::*;

#[derive(Debug, Clone, Copy)]
enum FixtureLayout {
    VideoAndAudio,
    VideoOnly,
    AudioOnly,
    Coupled,
}

fn parent(
    source_value: u64,
    extraction_generation: u64,
    exact_key: &str,
) -> ExactSelectionIdentity {
    let source = SourceIdentity::new(source_value);
    ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(extraction_generation),
            CandidateFormatIdentity::new(exact_key).expect("fixture parent key валиден"),
        ),
        SemanticIdentity::new(source, format!("semantic-{exact_key}"))
            .expect("fixture semantic parent валиден"),
    )
    .expect("fixture parent source совпадает")
}

fn catalog_identity(
    parent: ExactSelectionIdentity,
    catalog_generation: u64,
) -> ComponentVariantCatalogIdentity {
    ComponentVariantCatalogIdentity::new(
        parent,
        ComponentVariantCatalogGeneration::new(catalog_generation),
    )
}

fn video_variant(
    identity: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    height: u32,
) -> VideoComponentVariant {
    let codec_identity = if exact_key.starts_with("https://") {
        "https://codec.secret/path?token=codec-parameters"
    } else {
        "vp09.00.51.08"
    };
    VideoComponentVariant::new(
        ComponentVariantExactIdentity::new(
            identity.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new(exact_key).expect("fixture video exact key валиден"),
        ),
        ComponentVariantSemanticIdentity::new(
            identity.parent().semantic().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new(semantic_key)
                .expect("fixture video semantic key валиден"),
        ),
        VideoTrackDescriptor::new(
            NormalizedCodec::parse(
                RawCodecIdentity::new(codec_identity)
                    .expect("fixture video codec identity валидна"),
            ),
            Some(VideoWidth::new(height * 16 / 9).expect("fixture width валиден")),
            Some(VideoHeight::new(height).expect("fixture height валиден")),
            Some(FrameRate::new(60, 1).expect("fixture fps валиден")),
            Some(Bitrate::new(u64::from(height) * 4_000).expect("fixture bitrate валиден")),
            DynamicRange::Sdr,
        ),
    )
}

fn audio_variant(
    identity: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    bitrate: u64,
) -> AudioComponentVariant {
    AudioComponentVariant::new(
        ComponentVariantExactIdentity::new(
            identity.clone(),
            ComponentKind::Audio,
            ComponentVariantExactKey::new(exact_key).expect("fixture audio exact key валиден"),
        ),
        ComponentVariantSemanticIdentity::new(
            identity.parent().semantic().clone(),
            ComponentKind::Audio,
            ComponentVariantSemanticKey::new(semantic_key)
                .expect("fixture audio semantic key валиден"),
        ),
        AudioTrackDescriptor::new(
            NormalizedCodec::parse(
                RawCodecIdentity::new("opus").expect("fixture audio codec identity валидна"),
            ),
            Some(SampleRate::new(48_000).expect("fixture sample rate валиден")),
            Some(ChannelCount::new(2).expect("fixture channels валидны")),
            Some(Bitrate::new(bitrate).expect("fixture audio bitrate валиден")),
            Some(
                LanguageTag::new("https://secret.example/audio?token=language")
                    .expect("fixture language валиден"),
            ),
        ),
    )
}

fn coupled_variant(
    identity: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    height: u32,
    audio_bitrate: u64,
) -> CoupledComponentVariant {
    let video = video_variant(identity, "coupled-video", "coupled-video-semantic", height);
    let audio = audio_variant(
        identity,
        "coupled-audio",
        "coupled-audio-semantic",
        audio_bitrate,
    );
    CoupledComponentVariant::new(
        CoupledVariantExactIdentity::new(
            identity.clone(),
            ComponentVariantExactKey::new(exact_key).expect("fixture coupled exact key валиден"),
        ),
        CoupledVariantSemanticIdentity::new(
            identity.parent().semantic().clone(),
            ComponentVariantSemanticKey::new(semantic_key)
                .expect("fixture coupled semantic key валиден"),
        ),
        video.track().clone(),
        audio.track().clone(),
    )
}

fn catalog(
    identity: ComponentVariantCatalogIdentity,
    layout: FixtureLayout,
) -> ComponentVariantCatalog {
    let entries = match layout {
        FixtureLayout::VideoAndAudio => ComponentVariantCatalogEntries::VideoAndAudio {
            video: vec![
                video_variant(
                    &identity,
                    "https://secret.example/video-720?token=one",
                    "video-semantic-720",
                    720,
                ),
                video_variant(&identity, "video-1080-secret", "video-semantic-1080", 1080),
            ],
            audio: vec![
                audio_variant(
                    &identity,
                    "https://secret.example/audio-a?token=two",
                    "audio-semantic-a",
                    128_000,
                ),
                audio_variant(&identity, "audio-b-secret", "audio-semantic-b", 256_000),
            ],
        },
        FixtureLayout::VideoOnly => ComponentVariantCatalogEntries::VideoOnly {
            video: vec![
                video_variant(&identity, "video-720", "video-semantic-720", 720),
                video_variant(&identity, "video-1080", "video-semantic-1080", 1080),
            ],
        },
        FixtureLayout::AudioOnly => ComponentVariantCatalogEntries::AudioOnly {
            audio: vec![
                audio_variant(&identity, "audio-a", "audio-semantic-a", 128_000),
                audio_variant(&identity, "audio-b", "audio-semantic-b", 256_000),
            ],
        },
        FixtureLayout::Coupled => ComponentVariantCatalogEntries::Topology {
            video: Vec::new(),
            audio: Vec::new(),
            compatibility: ComponentVariantCompatibilityEntries::Unavailable,
            coupled: vec![
                coupled_variant(
                    &identity,
                    "https://secret.example/coupled-720?token=four",
                    "coupled-semantic-720",
                    720,
                    128_000,
                ),
                coupled_variant(
                    &identity,
                    "coupled-1080-secret",
                    "coupled-semantic-1080",
                    1080,
                    256_000,
                ),
            ],
            video_only: Vec::new(),
            audio_only: Vec::new(),
        },
    };
    ComponentVariantCatalog::new(
        identity,
        ComponentVariantCatalogLimit::new(16).expect("fixture catalog limit валиден"),
        entries,
    )
    .expect("fixture catalog валиден")
}

fn selection(
    catalog: &ComponentVariantCatalog,
    video_index: usize,
    audio_index: usize,
) -> ComponentVariantSelection {
    let request = match catalog {
        ComponentVariantCatalog::Topology { coupled, .. } if !coupled.is_empty() => {
            ComponentVariantSelectionRequest::Coupled {
                presentation: coupled[video_index].exact_identity().clone(),
            }
        }
        ComponentVariantCatalog::Topology { video, audio, .. }
        | ComponentVariantCatalog::VideoAndAudio { video, audio, .. } => {
            ComponentVariantSelectionRequest::VideoAndAudio {
                video: video[video_index].exact_identity().clone(),
                audio: audio[audio_index].exact_identity().clone(),
            }
        }
        ComponentVariantCatalog::VideoOnly { video, .. } => {
            ComponentVariantSelectionRequest::VideoOnly {
                video: video[video_index].exact_identity().clone(),
            }
        }
        ComponentVariantCatalog::AudioOnly { audio, .. } => {
            ComponentVariantSelectionRequest::AudioOnly {
                audio: audio[audio_index].exact_identity().clone(),
            }
        }
    };
    catalog
        .select_exact(request)
        .expect("fixture selection валиден")
}

pub(crate) fn configuration_for(parent: ExactSelectionIdentity) -> WebMediaStreamConfiguration {
    let generation = WebMediaStreamGeneration {
        source: parent.exact().source().value(),
        extraction: parent.exact().generation().value(),
    };
    let active_candidate = super::tests::candidate(Some(1080), false);
    WebMediaStreamConfiguration {
        generation,
        active_parent: parent,
        active_parent_selection: ActiveParentCandidateSelection::ProjectionFixture,
        candidates: Arc::from([active_candidate.clone()]),
        candidate_selections: Arc::from([]),
        active_candidate,
        preference: WebMediaSelectionPreference::GlobalPreferredHeight(1080),
        component_variants: WebMediaComponentVariantConfiguration::Unavailable,
        hls_subtitle_renditions: Arc::from([]),
    }
}

#[test]
fn default_and_muxed_without_provider_catalog_remain_honestly_unavailable() {
    assert_eq!(
        WebMediaComponentVariantConfiguration::default(),
        WebMediaComponentVariantConfiguration::Unavailable
    );
    let configuration = configuration_for(parent(1, 7, "muxed-active"));
    assert_eq!(
        configuration.component_variant_projection(),
        WebMediaComponentVariantProjection::Unavailable
    );
    assert_eq!(
        configuration.component_selection_reopen_intent(),
        crate::web_media_open::YtDlpComponentSelectionOpenIntent::ProviderDefault
    );
    assert_eq!(
        configuration
            .neutral_selection()
            .expect("parent-only neutral selection")
            .shape_kind(),
        web_media_core::WebMediaSelectionShapeKind::Candidate
    );
}

#[test]
fn language_display_projection_keeps_locale_labels_and_drops_secret_shaped_metadata() {
    assert_eq!(safe_language_tag("uk").as_deref(), Some("uk"));
    assert_eq!(safe_language_tag("en-US").as_deref(), Some("en-US"));
    assert_eq!(
        safe_language_tag("zh-Hant-TW").as_deref(),
        Some("zh-Hant-TW")
    );
    assert_eq!(safe_language_tag("BearerSecretToken"), None);
    assert_eq!(
        safe_language_tag("https://secret.example/audio?token=language"),
        None
    );
}

#[test]
fn matching_catalog_install_canonicalizes_supplied_selection() {
    let active_parent = parent(1, 7, "active");
    let identity = catalog_identity(active_parent.clone(), 3);
    let canonical_catalog = Arc::new(catalog(identity.clone(), FixtureLayout::VideoAndAudio));
    let supplied_catalog = catalog(identity, FixtureLayout::VideoAndAudio);
    let supplied_selection = selection(&supplied_catalog, 1, 0);

    let configured = configuration_for(active_parent.clone())
        .with_component_variants(Arc::clone(&canonical_catalog), supplied_selection)
        .expect("matching catalog должен установиться");

    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoAndAudio { video, audio, .. },
    ) = configured.component_variant_projection()
    else {
        panic!("ожидалась shape V+A");
    };
    assert_eq!(video.active_index, 1);
    assert_eq!(audio.active_index, 0);
    assert_eq!(video.variants[1].height, Some(1080));
    let neutral_selection = configured
        .neutral_selection()
        .expect("installed components form neutral selection");
    assert_eq!(
        neutral_selection.shape_kind(),
        web_media_core::WebMediaSelectionShapeKind::Components
    );
    assert_eq!(neutral_selection.parent(), &active_parent);
    assert_eq!(
        configured.component_selection_reopen_intent(),
        crate::web_media_open::YtDlpComponentSelectionOpenIntent::Semantic(
            selection(&canonical_catalog, 1, 0).semantic_rematch_request()
        )
    );
}

#[test]
fn install_rejects_cross_parent_stale_catalog_and_wrong_layout_with_typed_errors() {
    let active_parent = parent(1, 7, "active");
    let target_identity = catalog_identity(active_parent.clone(), 3);
    let target_catalog = Arc::new(catalog(target_identity.clone(), FixtureLayout::VideoOnly));

    let cross_parent_catalog = catalog(
        catalog_identity(parent(1, 7, "other-parent"), 3),
        FixtureLayout::VideoOnly,
    );
    let cross_parent_selection = selection(&cross_parent_catalog, 0, 0);
    assert_eq!(
        configuration_for(active_parent.clone())
            .with_component_variants(Arc::clone(&target_catalog), cross_parent_selection,),
        Err(ComponentVariantInstallationError::InvalidSelection(
            ComponentVariantError::CrossParent,
        ))
    );

    let stale_catalog = catalog(
        catalog_identity(active_parent.clone(), 2),
        FixtureLayout::VideoOnly,
    );
    let stale_selection = selection(&stale_catalog, 0, 0);
    assert!(matches!(
        configuration_for(active_parent.clone())
            .with_component_variants(Arc::clone(&target_catalog), stale_selection),
        Err(ComponentVariantInstallationError::InvalidSelection(
            ComponentVariantError::StaleCatalogGeneration { .. }
        ))
    ));

    let wrong_layout_catalog = catalog(target_identity, FixtureLayout::AudioOnly);
    let wrong_layout_selection = selection(&wrong_layout_catalog, 0, 0);
    assert_eq!(
        configuration_for(active_parent)
            .with_component_variants(Arc::clone(&target_catalog), wrong_layout_selection,),
        Err(ComponentVariantInstallationError::InvalidSelection(
            ComponentVariantError::LayoutMismatch,
        ))
    );
}

#[test]
fn install_rejects_catalog_for_non_active_parent_before_selection_lookup() {
    let active_parent = parent(1, 7, "active");
    let foreign_catalog = Arc::new(catalog(
        catalog_identity(parent(1, 7, "foreign"), 3),
        FixtureLayout::VideoOnly,
    ));
    let foreign_selection = selection(&foreign_catalog, 0, 0);
    assert_eq!(
        configuration_for(active_parent)
            .with_component_variants(foreign_catalog, foreign_selection),
        Err(ComponentVariantInstallationError::ActiveParentMismatch)
    );
}

#[test]
fn action_validation_order_covers_stale_parent_catalog_axis_and_index() {
    let active_parent = parent(2, 8, "active");
    let catalog_generation = ComponentVariantCatalogGeneration::new(5);
    let variants = Arc::new(catalog(
        ComponentVariantCatalogIdentity::new(active_parent.clone(), catalog_generation),
        FixtureLayout::VideoOnly,
    ));
    let configured = configuration_for(active_parent)
        .with_component_variants(Arc::clone(&variants), selection(&variants, 0, 0))
        .expect("fixture catalog должен установиться");
    let generation = configured.generation();

    let action = |parent_generation, catalog_generation, axis, variant_index| {
        ComponentVariantSelectionAction {
            parent_generation,
            catalog_generation,
            axis,
            variant_index,
        }
    };
    assert!(matches!(
        configured.resolve_component_variant_action(action(
            WebMediaStreamGeneration {
                source: generation.source,
                extraction: generation.extraction - 1,
            },
            ComponentVariantCatalogGeneration::new(4),
            WebMediaComponentVariantAxisKind::Audio,
            99,
        )),
        Err(ComponentVariantActionError::StaleParentGeneration { .. })
    ));
    assert!(matches!(
        configured.resolve_component_variant_action(action(
            generation,
            ComponentVariantCatalogGeneration::new(4),
            WebMediaComponentVariantAxisKind::Audio,
            99,
        )),
        Err(ComponentVariantActionError::StaleCatalogGeneration { .. })
    ));
    assert_eq!(
        configured.resolve_component_variant_action(action(
            generation,
            catalog_generation,
            WebMediaComponentVariantAxisKind::Audio,
            99,
        )),
        Err(ComponentVariantActionError::WrongAxis {
            axis: WebMediaComponentVariantAxisKind::Audio,
        })
    );
    assert!(matches!(
        configured.resolve_component_variant_action(action(
            generation,
            catalog_generation,
            WebMediaComponentVariantAxisKind::Video,
            99,
        )),
        Err(ComponentVariantActionError::VariantIndexOutOfRange { .. })
    ));
}

#[test]
fn active_index_is_no_change_and_replacements_preserve_other_axis() {
    let active_parent = parent(3, 9, "active");
    let catalog_generation = ComponentVariantCatalogGeneration::new(6);
    let variants = Arc::new(catalog(
        ComponentVariantCatalogIdentity::new(active_parent.clone(), catalog_generation),
        FixtureLayout::VideoAndAudio,
    ));
    let configured = configuration_for(active_parent)
        .with_component_variants(Arc::clone(&variants), selection(&variants, 0, 0))
        .expect("fixture catalog должен установиться");
    let generation = configured.generation();
    let preference_before = configured.preference();

    let resolve = |axis, variant_index| {
        configured.resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: generation,
            catalog_generation,
            axis,
            variant_index,
        })
    };
    assert_eq!(
        resolve(WebMediaComponentVariantAxisKind::Video, 0),
        Ok(ComponentVariantActionResolution::NoChange)
    );

    let ComponentVariantActionResolution::SemanticReopen(
        ComponentVariantSemanticSelectionRequest::VideoAndAudio {
            video: replaced_video,
            audio: retained_audio,
        },
    ) = resolve(WebMediaComponentVariantAxisKind::Video, 1).expect("video replacement валиден")
    else {
        panic!("ожидался semantic V+A reopen");
    };
    assert_eq!(
        replaced_video,
        variants.required_video_variants().expect("video axis")[1]
            .semantic_identity()
            .clone()
    );
    assert_eq!(
        retained_audio,
        variants.required_audio_variants().expect("audio axis")[0]
            .semantic_identity()
            .clone()
    );

    let ComponentVariantActionResolution::SemanticReopen(
        ComponentVariantSemanticSelectionRequest::VideoAndAudio {
            video: retained_video,
            audio: replaced_audio,
        },
    ) = resolve(WebMediaComponentVariantAxisKind::Audio, 1).expect("audio replacement валиден")
    else {
        panic!("ожидался semantic V+A reopen");
    };
    assert_eq!(
        retained_video,
        variants.required_video_variants().expect("video axis")[0]
            .semantic_identity()
            .clone()
    );
    assert_eq!(
        replaced_audio,
        variants.required_audio_variants().expect("audio axis")[1]
            .semantic_identity()
            .clone()
    );
    assert_eq!(configured.preference(), preference_before);
}

#[test]
fn coupled_axis_is_atomic_and_reopens_only_the_selected_semantic_row() {
    let active_parent = parent(5, 11, "active-coupled");
    let catalog_generation = ComponentVariantCatalogGeneration::new(8);
    let variants = Arc::new(catalog(
        ComponentVariantCatalogIdentity::new(active_parent.clone(), catalog_generation),
        FixtureLayout::Coupled,
    ));
    let configured = configuration_for(active_parent)
        .with_component_variants(Arc::clone(&variants), selection(&variants, 0, 0))
        .expect("coupled catalog должен установиться без fake split");
    let generation = configured.generation();
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
    ) = configured.component_variant_projection()
    else {
        panic!("ожидалась единая coupled A/V axis");
    };
    assert_eq!(coupled.active_index, 0);
    assert_eq!(coupled.variants.len(), 2);
    assert_eq!(coupled.variants[0].video.height, Some(720));
    assert_eq!(coupled.variants[0].audio.bitrate, Some(128_000));

    let resolve = |axis, variant_index| {
        configured.resolve_component_variant_action(ComponentVariantSelectionAction {
            parent_generation: generation,
            catalog_generation,
            axis,
            variant_index,
        })
    };
    assert_eq!(
        resolve(WebMediaComponentVariantAxisKind::Video, 1),
        Err(ComponentVariantActionError::WrongAxis {
            axis: WebMediaComponentVariantAxisKind::Video,
        })
    );
    assert_eq!(
        resolve(WebMediaComponentVariantAxisKind::Coupled, 0),
        Ok(ComponentVariantActionResolution::NoChange)
    );
    let ComponentVariantActionResolution::SemanticReopen(
        ComponentVariantSemanticSelectionRequest::Coupled { presentation },
    ) = resolve(WebMediaComponentVariantAxisKind::Coupled, 1)
        .expect("coupled replacement должен дать semantic reopen")
    else {
        panic!("ожидался semantic coupled reopen");
    };
    assert_eq!(
        presentation,
        variants.coupled_presentations()[1]
            .semantic_identity()
            .clone()
    );
}

#[test]
fn presentation_shapes_are_additive_and_secret_safe() {
    for layout in [
        FixtureLayout::VideoAndAudio,
        FixtureLayout::VideoOnly,
        FixtureLayout::AudioOnly,
        FixtureLayout::Coupled,
    ] {
        let active_parent = parent(4, 10, "https://secret.example/parent?token=three");
        let variants = Arc::new(catalog(catalog_identity(active_parent.clone(), 7), layout));
        let configured = configuration_for(active_parent)
            .with_component_variants(Arc::clone(&variants), selection(&variants, 0, 0))
            .expect("fixture catalog должен установиться");
        let projection = configured.component_variant_projection();

        match (&projection, layout) {
            (
                WebMediaComponentVariantProjection::Installed(
                    WebMediaInstalledComponentVariantPresentation::VideoAndAudio {
                        video,
                        audio,
                        ..
                    },
                ),
                FixtureLayout::VideoAndAudio,
            ) => {
                assert_eq!(video.variants.len() + audio.variants.len(), 4);
                assert_eq!(video.variants.len() * audio.variants.len(), 4);
            }
            (
                WebMediaComponentVariantProjection::Installed(
                    WebMediaInstalledComponentVariantPresentation::VideoOnly { .. },
                ),
                FixtureLayout::VideoOnly,
            )
            | (
                WebMediaComponentVariantProjection::Installed(
                    WebMediaInstalledComponentVariantPresentation::AudioOnly { .. },
                ),
                FixtureLayout::AudioOnly,
            ) => {}
            (
                WebMediaComponentVariantProjection::Installed(
                    WebMediaInstalledComponentVariantPresentation::Coupled { coupled, .. },
                ),
                FixtureLayout::Coupled,
            ) => {
                assert_eq!(coupled.variants.len(), 2);
            }
            _ => panic!("projection должна сохранять catalog shape"),
        }

        let debug = format!("{configured:?} {projection:?}");
        for secret in [
            "secret.example",
            "token=",
            "video-720?token",
            "audio-a?token",
            "semantic-720",
            "codec.secret",
            "codec-parameters",
        ] {
            assert!(
                !debug.contains(secret),
                "safe Debug не должен содержать sentinel {secret}"
            );
        }
    }
}
