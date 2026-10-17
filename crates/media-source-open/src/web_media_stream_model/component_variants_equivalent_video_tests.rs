//! Смена версии звука в Topology-каталоге с «привязанными» audio group (Dolby HLS по Apple
//! authoring spec): видимая картинка сохраняется через эквивалентный video row, иначе — прежний
//! typed отказ без смены качества.

use std::sync::Arc;

use web_media_core::{
    ComponentVariantCatalog, ComponentVariantCatalogEntries, ComponentVariantCatalogGeneration,
    ComponentVariantCatalogIdentity, ComponentVariantCatalogLimit,
    ComponentVariantCompatibilityEdge, ComponentVariantCompatibilityEntries,
    ComponentVariantEdgeLimit, ComponentVariantError, ComponentVariantSelectionRequest,
    ComponentVariantSemanticSelectionRequest,
};

use super::component_variants::{
    ComponentVariantActionError, ComponentVariantActionResolution, ComponentVariantSelectionAction,
    WebMediaComponentVariantAxisKind,
};
use super::component_variants_tests::{audio_variant, configuration_for, parent, video_variant};

const CATALOG_GENERATION: u64 = 31;

/// Видео 720 «под AAC», видео 1080 «под E-AC-3», видео 720 «под E-AC-3» и два звука.
///
/// Ряд 1080 стоит раньше эквивалентного 720: правило обязано выбрать ту же картинку, а не
/// первый совместимый ряд каталога.
struct BoundGroupsCatalog {
    catalog: Arc<ComponentVariantCatalog>,
}

const AAC_VIDEO: usize = 0;
const OTHER_PICTURE_EAC3_VIDEO: usize = 1;
const SAME_PICTURE_EAC3_VIDEO: usize = 2;
const AAC_AUDIO: usize = 0;
const EAC3_AUDIO: usize = 1;

fn bound_groups_catalog(include_same_picture_row: bool) -> BoundGroupsCatalog {
    let identity = ComponentVariantCatalogIdentity::new(
        parent(4, 2, "native-hls-vod"),
        ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
    );
    let mut video = vec![
        video_variant(&identity, "video-aac-720", "video-aac-720", 720),
        video_variant(&identity, "video-ec3-1080", "video-ec3-1080", 1080),
    ];
    if include_same_picture_row {
        video.push(video_variant(
            &identity,
            "video-ec3-720",
            "video-ec3-720",
            720,
        ));
    }
    let audio = vec![
        audio_variant(&identity, "audio-aac", "audio-aac", 128_000),
        audio_variant(&identity, "audio-ec3", "audio-ec3", 384_000),
    ];
    let edge = |video_index: usize, audio_index: usize| {
        ComponentVariantCompatibilityEdge::new(
            video[video_index].exact_identity().clone(),
            audio[audio_index].exact_identity().clone(),
        )
    };
    let mut edges = vec![
        edge(AAC_VIDEO, AAC_AUDIO),
        edge(OTHER_PICTURE_EAC3_VIDEO, EAC3_AUDIO),
    ];
    if include_same_picture_row {
        edges.push(edge(SAME_PICTURE_EAC3_VIDEO, EAC3_AUDIO));
    }
    let catalog = ComponentVariantCatalog::new(
        identity,
        ComponentVariantCatalogLimit::new(16).expect("fixture catalog limit"),
        ComponentVariantCatalogEntries::Topology {
            video,
            audio,
            compatibility: ComponentVariantCompatibilityEntries::Sparse {
                edge_limit: ComponentVariantEdgeLimit::new(8).expect("fixture edge limit"),
                edges,
            },
            coupled: Vec::new(),
            video_only: Vec::new(),
            audio_only: Vec::new(),
        },
    )
    .expect("fixture topology catalog");
    BoundGroupsCatalog {
        catalog: Arc::new(catalog),
    }
}

/// Резолвит выбор E-AC-3 при установленной паре «720 + AAC».
fn resolve_eac3_from_aac(
    fixture: &BoundGroupsCatalog,
) -> Result<ComponentVariantActionResolution, ComponentVariantActionError> {
    let catalog = &fixture.catalog;
    let video = catalog.required_video_variants().expect("video axis");
    let audio = catalog.required_audio_variants().expect("audio axis");
    let installed = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: video[AAC_VIDEO].exact_identity().clone(),
            audio: audio[AAC_AUDIO].exact_identity().clone(),
        })
        .expect("AAC pair is compatible");
    let configured = configuration_for(parent(4, 2, "native-hls-vod"))
        .with_component_variants(Arc::clone(catalog), installed)
        .expect("fixture catalog installs");
    configured.resolve_component_variant_action(ComponentVariantSelectionAction {
        parent_generation: configured.generation(),
        catalog_generation: ComponentVariantCatalogGeneration::new(CATALOG_GENERATION),
        axis: WebMediaComponentVariantAxisKind::Audio,
        variant_index: EAC3_AUDIO,
    })
}

#[test]
fn audio_switch_moves_to_the_equivalent_picture_row_bound_to_the_requested_audio() {
    let fixture = bound_groups_catalog(true);
    let Ok(ComponentVariantActionResolution::SemanticReopen(
        ComponentVariantSemanticSelectionRequest::VideoAndAudio { video, audio },
    )) = resolve_eac3_from_aac(&fixture)
    else {
        panic!("E-AC-3 must be reachable through the equivalent 720p row");
    };
    let catalog_video = fixture.catalog.required_video_variants().expect("video");
    let catalog_audio = fixture.catalog.required_audio_variants().expect("audio");
    assert_eq!(
        video,
        catalog_video[SAME_PICTURE_EAC3_VIDEO]
            .semantic_identity()
            .clone(),
        "the picture stays 720p; the 1080p row listed earlier is not chosen"
    );
    assert_eq!(audio, catalog_audio[EAC3_AUDIO].semantic_identity().clone());
}

#[test]
fn audio_switch_without_an_equivalent_picture_keeps_the_typed_incompatibility() {
    let fixture = bound_groups_catalog(false);
    assert_eq!(
        resolve_eac3_from_aac(&fixture),
        Err(ComponentVariantActionError::ReplacementFailed(
            ComponentVariantError::IncompatibleComponentPair
        )),
        "the video quality is never changed silently for an audio choice"
    );
}
