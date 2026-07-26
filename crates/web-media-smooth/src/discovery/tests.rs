use std::num::NonZeroUsize;
use std::sync::Arc;

use demux_api::DemuxSniffBudget;
use media_core::TrackInfo;
use rustiplayer_config::NetworkConfig;
use source_core::SourceRuntimeConfig;
use web_media_core::{
    ComponentKind, ComponentVariantCatalogGeneration, ComponentVariantSelectionRequest,
    PreferredHeightPolicy,
};

use super::{
    SmoothCatalogDiscoveryPolicy, SmoothCatalogDiscoveryRequest, SmoothComponentCapabilityProbe,
    SmoothComponentCapabilityRejection, discover_smooth_vod_catalog,
};
use crate::demux::tests::{TestSymphoniaFactory, demux_policy};
use crate::source::tests::{FixtureOrigin, fragment_policy, preparation_policy, transport_request};
use crate::{SmoothPrepareRequest, SmoothSiblingRejectionReason, prepare_smooth_vod};

struct AcceptAllCapabilities;

impl SmoothComponentCapabilityProbe for AcceptAllCapabilities {
    fn check_video(&self, _track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection> {
        Ok(())
    }

    fn check_audio(&self, _track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection> {
        Ok(())
    }
}

struct RejectFullHdCapabilities;

impl SmoothComponentCapabilityProbe for RejectFullHdCapabilities {
    fn check_video(&self, track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection> {
        if track
            .video
            .as_ref()
            .and_then(|video| video.coded_height)
            .is_some_and(|height| height >= 700)
        {
            Err(SmoothComponentCapabilityRejection)
        } else {
            Ok(())
        }
    }

    fn check_audio(&self, _track: &TrackInfo) -> Result<(), SmoothComponentCapabilityRejection> {
        Ok(())
    }
}

fn discovery_policy() -> SmoothCatalogDiscoveryPolicy {
    SmoothCatalogDiscoveryPolicy::new(
        fragment_policy(),
        DemuxSniffBudget::new(
            NonZeroUsize::new(256 * 1_024).expect("sniff bytes"),
            NonZeroUsize::new(2).expect("sniff segments"),
            std::time::Duration::from_secs(2),
        )
        .expect("sniff policy"),
        NonZeroUsize::new(8).expect("probe events"),
    )
}

fn discover(
    origin: &FixtureOrigin,
    generation: u64,
    capabilities: &dyn SmoothComponentCapabilityProbe,
) -> super::SmoothDiscoveredCatalog {
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    discover_smooth_vod_catalog(SmoothCatalogDiscoveryRequest::new(
        SmoothPrepareRequest::new(
            transport_request(origin.target()),
            &source_config,
            ComponentVariantCatalogGeneration::new(generation),
            PreferredHeightPolicy::NoPreference,
            preparation_policy(),
        ),
        Arc::new(TestSymphoniaFactory),
        capabilities,
        discovery_policy(),
    ))
    .expect("Smooth discovery")
}

#[test]
fn provider_default_preparation_fetches_no_sibling_content_and_publishes_one_pair() {
    let origin = FixtureOrigin::start();
    let source_config =
        SourceRuntimeConfig::from_network_config(&NetworkConfig::default()).expect("source config");
    let prepared = prepare_smooth_vod(SmoothPrepareRequest::new(
        transport_request(origin.target()),
        &source_config,
        ComponentVariantCatalogGeneration::new(1),
        PreferredHeightPolicy::NoPreference,
        preparation_policy(),
    ))
    .expect("fast default preparation");

    assert_eq!(
        prepared.catalog().required_video_variants().unwrap().len(),
        1
    );
    assert_eq!(
        prepared.catalog().required_audio_variants().unwrap().len(),
        1
    );
    assert_eq!(prepared.runtime_seed.video_rows.len(), 1);
    assert_eq!(prepared.runtime_seed.audio_rows.len(), 1);
    assert_eq!(origin.request_count(), 1, "only Manifest may be fetched");
}

#[test]
fn discovery_isolates_content_and_capability_failures_before_atomic_all_pairs_publish() {
    let origin = FixtureOrigin::start();
    let discovered = discover(&origin, 2, &RejectFullHdCapabilities);

    assert_eq!(
        discovered
            .catalog()
            .required_video_variants()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        discovered
            .catalog()
            .required_audio_variants()
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        discovered
            .catalog()
            .compatibility()
            .expect("AllPairs compatibility")
            .logical_edge_count(),
        1
    );
    assert!(discovered.sibling_rejections().iter().any(|rejection| {
        rejection.component() == ComponentKind::Video
            && rejection.reason() == SmoothSiblingRejectionReason::CapabilityUnavailable
    }));
    assert!(discovered.sibling_rejections().iter().any(|rejection| {
        rejection.component() == ComponentKind::Video
            && rejection.reason() == SmoothSiblingRejectionReason::TransportOrContentUnavailable
    }));
}

#[test]
fn fresh_catalog_rejects_stale_exact_and_provider_reopens_semantic_components() {
    let first_origin = FixtureOrigin::start();
    let first = discover(&first_origin, 3, &AcceptAllCapabilities);
    let selected = first.provider_default_selection().clone();
    let stale_exact = selected.exact_selection_request();
    let semantic = selected.semantic_rematch_request();

    let fresh_origin = FixtureOrigin::start();
    let fresh = discover(&fresh_origin, 4, &AcceptAllCapabilities);
    assert!(fresh.catalog().select_exact(stale_exact).is_err());
    let opened = fresh
        .open_semantic(semantic.clone(), fragment_policy(), demux_policy())
        .expect("semantic reopen uses fresh private rows");
    assert_eq!(opened.selection().semantic_rematch_request(), semantic);
    assert!(matches!(
        opened.selection().exact_selection_request(),
        ComponentVariantSelectionRequest::VideoAndAudio { .. }
    ));
}

#[test]
fn provider_exact_reopen_preserves_selected_catalog_and_receipted_runtime() {
    let origin = FixtureOrigin::start();
    let discovered = discover(&origin, 5, &AcceptAllCapabilities);
    let exact = discovered
        .provider_default_selection()
        .exact_selection_request();
    let opened = discovered
        .open_exact(exact, fragment_policy(), demux_policy())
        .expect("exact reopen uses retained private rows");

    assert_eq!(opened.catalog().required_video_variants().unwrap().len(), 2);
    assert_eq!(
        opened.async_seek_handle().runtime_generation(),
        demux_api::ProgressiveRuntimeGeneration::new(17)
    );
}
