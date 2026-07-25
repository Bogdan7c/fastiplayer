use std::sync::Arc;

use web_media_core::{
    Bitrate, CandidateFormatIdentity, CandidateIdentity, ComponentKind, ComponentVariantCatalog,
    ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit, ComponentVariantExactIdentity,
    ComponentVariantExactKey, ComponentVariantSelection, ComponentVariantSelectionRequest,
    ComponentVariantSemanticIdentity, ComponentVariantSemanticKey, DynamicRange,
    ExactSelectionIdentity, ExtractionGeneration, NormalizedCodec, RawCodecIdentity,
    SemanticIdentity, SourceIdentity, VideoComponentVariant, VideoHeight, VideoTrackDescriptor,
    VideoWidth,
};

use crate::web_media_stream_model::{
    component_variants::{
        ComponentVariantInstallationError, WebMediaComponentVariantProjection,
        WebMediaInstalledComponentVariantPresentation,
    },
    component_variants_tests::configuration_for,
};

use super::component_variants::{
    ComponentVariantFinalizationError, PreparedComponentVariantCatalog,
    YtDlpComponentSelectionOpenIntent, finalize_component_variant_configuration,
};

/// Строит parent identity с отдельно управляемой semantic частью для fresh-generation fixtures.
fn parent(
    source_value: u64,
    extraction_generation: u64,
    exact_key: &str,
    semantic_key: &str,
) -> ExactSelectionIdentity {
    let source = SourceIdentity::new(source_value);
    ExactSelectionIdentity::new(
        CandidateIdentity::new(
            source,
            ExtractionGeneration::new(extraction_generation),
            CandidateFormatIdentity::new(exact_key).expect("fixture parent key валиден"),
        ),
        SemanticIdentity::new(source, semantic_key).expect("fixture parent semantic key валиден"),
    )
    .expect("fixture parent identities принадлежат одному source")
}

/// Строит две video rows одного catalog generation.
fn video_catalog(
    parent: ExactSelectionIdentity,
    catalog_generation: u64,
    first_semantic_key: &str,
    second_semantic_key: &str,
) -> ComponentVariantCatalog {
    let identity = ComponentVariantCatalogIdentity::new(
        parent,
        ComponentVariantCatalogGeneration::new(catalog_generation),
    );
    let first = video_variant(&identity, "fresh-exact-720", first_semantic_key, 720);
    let second = video_variant(&identity, "fresh-exact-1080", second_semantic_key, 1080);
    ComponentVariantCatalog::new(
        identity,
        ComponentVariantCatalogLimit::new(4).expect("fixture catalog limit валиден"),
        ComponentVariantCatalogEntries::VideoOnly {
            video: vec![first, second],
        },
    )
    .expect("fixture catalog валиден")
}

/// Строит одну immutable video row без URL-bearing presentation полей.
fn video_variant(
    catalog: &ComponentVariantCatalogIdentity,
    exact_key: &str,
    semantic_key: &str,
    height: u32,
) -> VideoComponentVariant {
    VideoComponentVariant::new(
        ComponentVariantExactIdentity::new(
            catalog.clone(),
            ComponentKind::Video,
            ComponentVariantExactKey::new(exact_key).expect("fixture exact key валиден"),
        ),
        ComponentVariantSemanticIdentity::new(
            catalog.parent().semantic().clone(),
            ComponentKind::Video,
            ComponentVariantSemanticKey::new(semantic_key).expect("fixture semantic key валиден"),
        ),
        VideoTrackDescriptor::new(
            NormalizedCodec::parse(
                RawCodecIdentity::new("vp09.00.51.08").expect("fixture codec валиден"),
            ),
            Some(VideoWidth::new(height * 16 / 9).expect("fixture width валиден")),
            Some(VideoHeight::new(height).expect("fixture height валиден")),
            None,
            Some(Bitrate::new(u64::from(height) * 4_000).expect("fixture bitrate валиден")),
            DynamicRange::Sdr,
        ),
    )
}

/// Выбирает exact video row через единственную core canonicalization boundary.
fn video_selection(
    catalog: &ComponentVariantCatalog,
    variant_index: usize,
) -> ComponentVariantSelection {
    let ComponentVariantCatalog::VideoOnly { video, .. } = catalog else {
        panic!("fixture обязан быть video-only");
    };
    catalog
        .select_exact(ComponentVariantSelectionRequest::VideoOnly {
            video: video[variant_index].exact_identity().clone(),
        })
        .expect("fixture selection валиден")
}

#[test]
fn provider_default_and_unavailable_preserve_honest_unavailable_configuration() {
    let active_parent = parent(1, 2, "parent", "stable-parent");
    let finalized = finalize_component_variant_configuration(
        configuration_for(active_parent),
        YtDlpComponentSelectionOpenIntent::ProviderDefault,
        PreparedComponentVariantCatalog::Unavailable,
    )
    .expect("provider default без catalog-а является валидной конфигурацией");

    assert_eq!(
        finalized.component_variant_projection(),
        WebMediaComponentVariantProjection::Unavailable
    );
}

#[test]
fn provider_default_installs_fresh_provider_selection() {
    let active_parent = parent(1, 2, "fresh-parent", "stable-parent");
    let catalog = Arc::new(video_catalog(
        active_parent.clone(),
        11,
        "stable-720",
        "stable-1080",
    ));
    let provider_selection = video_selection(&catalog, 1);
    let finalized = finalize_component_variant_configuration(
        configuration_for(active_parent),
        YtDlpComponentSelectionOpenIntent::ProviderDefault,
        PreparedComponentVariantCatalog::Installed {
            catalog,
            provider_selection,
        },
    )
    .expect("provider default selection должен установиться");

    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation,
            video,
        },
    ) = finalized.component_variant_projection()
    else {
        panic!("ожидалась установленная video-only projection");
    };
    assert_eq!(catalog_generation.value(), 11);
    assert_eq!(video.active_index, 1);
}

#[test]
fn semantic_and_unavailable_fail_before_publication_without_fallback() {
    let old_parent = parent(1, 1, "old-parent", "stable-parent");
    let old_catalog = video_catalog(old_parent, 7, "stable-720", "stable-1080");
    let semantic_request = video_selection(&old_catalog, 0).semantic_rematch_request();
    let fresh_parent = parent(1, 2, "fresh-parent", "stable-parent");
    let untouched_configuration = configuration_for(fresh_parent);

    let error = finalize_component_variant_configuration(
        untouched_configuration.clone(),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request),
        PreparedComponentVariantCatalog::Unavailable,
    )
    .expect_err("semantic intent не имеет права fallback-нуться");

    assert_eq!(
        error,
        ComponentVariantFinalizationError::ComponentCatalogUnavailable
    );
    assert_eq!(
        untouched_configuration.component_variant_projection(),
        WebMediaComponentVariantProjection::Unavailable
    );
}

#[test]
fn semantic_installed_rematches_fresh_exact_generation_not_provider_default() {
    let old_parent = parent(1, 1, "old-parent", "stable-parent");
    let old_catalog = video_catalog(old_parent, 4, "stable-720", "stable-1080");
    let semantic_request = video_selection(&old_catalog, 0).semantic_rematch_request();
    let fresh_parent = parent(1, 2, "fresh-parent", "stable-parent");
    let fresh_catalog = Arc::new(video_catalog(
        fresh_parent.clone(),
        12,
        "stable-1080",
        "stable-720",
    ));
    let provider_default = video_selection(&fresh_catalog, 0);

    let finalized = finalize_component_variant_configuration(
        configuration_for(fresh_parent),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request),
        PreparedComponentVariantCatalog::Installed {
            catalog: fresh_catalog,
            provider_selection: provider_default,
        },
    )
    .expect("semantic row должна rematch-нуться на fresh catalog");

    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly {
            catalog_generation,
            video,
        },
    ) = finalized.component_variant_projection()
    else {
        panic!("ожидалась установленная video-only projection");
    };
    assert_eq!(catalog_generation.value(), 12);
    assert_eq!(
        video.active_index, 1,
        "semantic stable-720 находится в fresh row 1, а provider default был row 0"
    );
}

#[test]
fn missing_semantic_variant_is_typed_and_does_not_publish_partial_configuration() {
    let old_parent = parent(1, 1, "old-parent", "stable-parent");
    let old_catalog = video_catalog(old_parent, 4, "requested-720", "requested-1080");
    let semantic_request = video_selection(&old_catalog, 0).semantic_rematch_request();
    let fresh_parent = parent(1, 2, "fresh-parent", "stable-parent");
    let fresh_catalog = Arc::new(video_catalog(
        fresh_parent.clone(),
        13,
        "different-720",
        "different-1080",
    ));
    let provider_default = video_selection(&fresh_catalog, 0);
    let untouched_configuration = configuration_for(fresh_parent);

    let error = finalize_component_variant_configuration(
        untouched_configuration.clone(),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request),
        PreparedComponentVariantCatalog::Installed {
            catalog: fresh_catalog,
            provider_selection: provider_default,
        },
    )
    .expect_err("отсутствующий semantic variant обязан завершить preparation");

    assert!(matches!(
        error,
        ComponentVariantFinalizationError::SemanticRematch(_)
    ));
    assert_eq!(
        untouched_configuration.component_variant_projection(),
        WebMediaComponentVariantProjection::Unavailable
    );
}

#[test]
fn cross_parent_install_failure_stays_typed() {
    let configured_parent = parent(1, 2, "configured-parent", "configured-semantic");
    let catalog_parent = parent(1, 2, "catalog-parent", "catalog-semantic");
    let catalog = Arc::new(video_catalog(
        catalog_parent,
        14,
        "stable-720",
        "stable-1080",
    ));
    let semantic_request = video_selection(&catalog, 0).semantic_rematch_request();
    let provider_default = video_selection(&catalog, 1);

    let error = finalize_component_variant_configuration(
        configuration_for(configured_parent),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request),
        PreparedComponentVariantCatalog::Installed {
            catalog,
            provider_selection: provider_default,
        },
    )
    .expect_err("catalog другого parent-а обязан быть отклонён");

    assert_eq!(
        error,
        ComponentVariantFinalizationError::Installation(
            ComponentVariantInstallationError::ActiveParentMismatch
        )
    );
}

#[test]
fn finalization_debug_and_errors_do_not_expose_component_keys() {
    let secret = "https://secret.example/component?token=do-not-log";
    let old_parent = parent(1, 1, "old-parent", "stable-parent");
    let old_catalog = video_catalog(old_parent, 4, secret, "other-semantic");
    let semantic_request = video_selection(&old_catalog, 0).semantic_rematch_request();
    let fresh_parent = parent(1, 2, "fresh-parent", "stable-parent");
    let fresh_catalog = Arc::new(video_catalog(
        fresh_parent.clone(),
        15,
        "different-semantic",
        "another-semantic",
    ));
    let provider_default = video_selection(&fresh_catalog, 0);

    let error = finalize_component_variant_configuration(
        configuration_for(fresh_parent),
        YtDlpComponentSelectionOpenIntent::Semantic(semantic_request),
        PreparedComponentVariantCatalog::Installed {
            catalog: fresh_catalog,
            provider_selection: provider_default,
        },
    )
    .expect_err("secret fixture специально не rematch-ится");

    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}
