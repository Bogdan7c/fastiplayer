//! Одноосевой выбор (`VideoOnly` / `AudioOnly`) в Topology-каталоге: sidebar видит только
//! строки из `video_only` / `audio_only`, а индексы действий считаются внутри этого пула.
//!
//! Это нужно DASH: в ffmpeg-MPD каталог получается Topology, а при пропаже одной оси
//! default-выбор честно деградирует в одноосевой.

use std::sync::Arc;

use web_media_core::{
    ComponentVariantCatalog, ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEdge, ComponentVariantCompatibilityEntries,
    ComponentVariantEdgeLimit, ComponentVariantSelection, ComponentVariantSelectionRequest,
    ComponentVariantSemanticSelectionRequest,
};

use super::WebMediaStreamConfiguration;
use super::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind, WebMediaComponentVariantProjection,
    WebMediaInstalledComponentVariantPresentation,
};
use super::component_variants_tests::{audio_variant, configuration_for, parent, video_variant};

const CATALOG_GENERATION: u64 = 41;

/// Видео: 720, 1080, 480; в `video_only` только 720 и 480 (1080 бывает лишь в паре).
/// Аудио: a, b, c; в `audio_only` только b и c.
fn topology_catalog() -> Arc<ComponentVariantCatalog> {
    let identity = ComponentVariantCatalogIdentity::new(
        parent(6, 3, "single-axis-topology"),
        ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
    );
    let video = vec![
        video_variant(&identity, "video-720", "video-semantic-720", 720),
        video_variant(&identity, "video-1080", "video-semantic-1080", 1080),
        video_variant(&identity, "video-480", "video-semantic-480", 480),
    ];
    let audio = vec![
        audio_variant(&identity, "audio-a", "audio-semantic-a", 64_000),
        audio_variant(&identity, "audio-b", "audio-semantic-b", 128_000),
        audio_variant(&identity, "audio-c", "audio-semantic-c", 256_000),
    ];
    let edge = ComponentVariantCompatibilityEdge::new(
        video[1].exact_identity().clone(),
        audio[0].exact_identity().clone(),
    );
    let video_only = vec![
        video[0].exact_identity().clone(),
        video[2].exact_identity().clone(),
    ];
    let audio_only = vec![
        audio[1].exact_identity().clone(),
        audio[2].exact_identity().clone(),
    ];
    Arc::new(
        ComponentVariantCatalog::new(
            identity,
            ComponentVariantCatalogLimit::new(16).expect("fixture catalog limit"),
            ComponentVariantCatalogEntries::Topology {
                video,
                audio,
                compatibility: ComponentVariantCompatibilityEntries::Sparse {
                    edge_limit: ComponentVariantEdgeLimit::new(8).expect("fixture edge limit"),
                    edges: vec![edge],
                },
                coupled: Vec::new(),
                video_only,
                audio_only,
            },
        )
        .expect("fixture topology catalog"),
    )
}

/// Ставит каталог и выбор; возвращает настроенную конфигурацию.
fn install(
    catalog: &Arc<ComponentVariantCatalog>,
    request: ComponentVariantSelectionRequest,
) -> WebMediaStreamConfiguration {
    let selection: ComponentVariantSelection = catalog
        .select_exact(request)
        .expect("fixture single-axis selection is allowed by the catalog");
    configuration_for(parent(6, 3, "single-axis-topology"))
        .with_component_variants(Arc::clone(catalog), selection)
        .expect("Topology catalog must accept a single-axis selection")
}

fn action(
    configured: &WebMediaStreamConfiguration,
    axis: WebMediaComponentVariantAxisKind,
    variant_index: usize,
) -> Result<ComponentVariantActionResolution, ComponentVariantActionError> {
    configured.resolve_component_variant_action(ComponentVariantSelectionAction {
        parent_generation: configured.generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
        axis,
        variant_index,
    })
}

fn video_only_request(
    catalog: &ComponentVariantCatalog,
    row: usize,
) -> ComponentVariantSelectionRequest {
    ComponentVariantSelectionRequest::VideoOnly {
        video: catalog.required_video_variants().expect("video axis")[row]
            .exact_identity()
            .clone(),
    }
}

fn audio_only_request(
    catalog: &ComponentVariantCatalog,
    row: usize,
) -> ComponentVariantSelectionRequest {
    ComponentVariantSelectionRequest::AudioOnly {
        audio: catalog.required_audio_variants().expect("audio axis")[row]
            .exact_identity()
            .clone(),
    }
}

#[test]
fn topology_video_only_publishes_only_video_only_rows_with_active_index_in_that_pool() {
    let catalog = topology_catalog();
    // Active = 480 (строка 2 каталога, позиция 1 в пуле [720, 480]).
    let configured = install(&catalog, video_only_request(&catalog, 2));

    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly { video, .. },
    ) = configured.component_variant_projection()
    else {
        panic!("Topology + VideoOnly must install the VideoOnly shape");
    };
    let heights = video
        .variants
        .iter()
        .map(|row| row.height)
        .collect::<Vec<_>>();
    assert_eq!(
        heights,
        vec![Some(720), Some(480)],
        "1080 is pair-only and not offered"
    );
    assert_eq!(video.active_index, 1);
    assert_eq!(
        configured
            .neutral_selection()
            .expect("installed single-axis selection")
            .shape_kind(),
        web_media_core::WebMediaSelectionShapeKind::Components
    );
}

#[test]
fn topology_video_only_actions_index_the_filtered_pool_and_never_reach_pair_only_rows() {
    let catalog = topology_catalog();
    let configured = install(&catalog, video_only_request(&catalog, 0));
    let video = catalog.required_video_variants().expect("video axis");

    // Active уже пул[0]: повтор -> NoChange.
    assert_eq!(
        action(&configured, WebMediaComponentVariantAxisKind::Video, 0),
        Ok(ComponentVariantActionResolution::NoChange)
    );
    // Пул[1] -- это строка 480 (каталог[2]), а не 1080 (каталог[1]).
    assert_eq!(
        action(&configured, WebMediaComponentVariantAxisKind::Video, 1),
        Ok(ComponentVariantActionResolution::SemanticReopen(
            ComponentVariantSemanticSelectionRequest::VideoOnly {
                video: video[2].semantic_identity().clone()
            }
        ))
    );
    // За пределами пула (индекс 2 существует в каталоге, но не в пуле) -- typed отказ.
    assert!(matches!(
        action(&configured, WebMediaComponentVariantAxisKind::Video, 2),
        Err(ComponentVariantActionError::VariantIndexOutOfRange {
            provided: 2,
            variant_count: 2,
            ..
        })
    ));
    // Оси audio в одноосевом video нет.
    assert!(matches!(
        action(&configured, WebMediaComponentVariantAxisKind::Audio, 0),
        Err(ComponentVariantActionError::WrongAxis { .. })
    ));
}

#[test]
fn topology_audio_only_publishes_filtered_pool_and_replaces_inside_it() {
    let catalog = topology_catalog();
    // Active = audio-b (строка 1, позиция 0 в пуле [b, c]).
    let configured = install(&catalog, audio_only_request(&catalog, 1));
    let audio = catalog.required_audio_variants().expect("audio axis");

    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::AudioOnly { audio: axis, .. },
    ) = configured.component_variant_projection()
    else {
        panic!("Topology + AudioOnly must install the AudioOnly shape");
    };
    let bitrates = axis
        .variants
        .iter()
        .map(|row| row.bitrate)
        .collect::<Vec<_>>();
    assert_eq!(
        bitrates,
        vec![Some(128_000), Some(256_000)],
        "audio-a is pair-only"
    );
    assert_eq!(axis.active_index, 0);

    assert_eq!(
        action(&configured, WebMediaComponentVariantAxisKind::Audio, 0),
        Ok(ComponentVariantActionResolution::NoChange)
    );
    assert_eq!(
        action(&configured, WebMediaComponentVariantAxisKind::Audio, 1),
        Ok(ComponentVariantActionResolution::SemanticReopen(
            ComponentVariantSemanticSelectionRequest::AudioOnly {
                audio: audio[2].semantic_identity().clone()
            }
        ))
    );
}

#[test]
fn flat_video_only_catalog_still_offers_every_row() {
    // Регрессия: плоский VideoOnly-каталог не должен терять строки из-за нового фильтра.
    let identity = ComponentVariantCatalogIdentity::new(
        parent(6, 3, "single-axis-topology"),
        ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
    );
    let catalog = Arc::new(
        ComponentVariantCatalog::new(
            identity.clone(),
            ComponentVariantCatalogLimit::new(16).expect("limit"),
            ComponentVariantCatalogEntries::VideoOnly {
                video: vec![
                    video_variant(&identity, "flat-720", "flat-semantic-720", 720),
                    video_variant(&identity, "flat-1080", "flat-semantic-1080", 1080),
                ],
            },
        )
        .expect("flat catalog"),
    );
    let configured = install(&catalog, video_only_request(&catalog, 1));
    let WebMediaComponentVariantProjection::Installed(
        WebMediaInstalledComponentVariantPresentation::VideoOnly { video, .. },
    ) = configured.component_variant_projection()
    else {
        panic!("flat VideoOnly shape expected");
    };
    assert_eq!(video.variants.len(), 2);
    assert_eq!(video.active_index, 1);
}
