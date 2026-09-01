//! Один atomic discovery/proof/publication pass logical DASH lanes.

use std::collections::BTreeMap;

use super::*;
use crate::plan::{prove_manifest_lane, prove_manifest_lane_alignment};

pub(super) fn build(
    request: DashRepresentationLaneCatalogBuildRequest<'_>,
    proof_port: &mut dyn DashRepresentationLaneProofPort,
) -> Result<DashRepresentationLaneCatalog, DashRepresentationLaneCatalogBuildError> {
    let Some(first_period) = request.presentation.periods.first() else {
        return Err(DashRepresentationLaneCatalogBuildError::EmptyPresentation);
    };
    let mut rejections = Vec::new();
    let mut first_contracts: BTreeMap<LaneContract, Vec<(usize, usize)>> = BTreeMap::new();
    for (adaptation_index, adaptation) in first_period.adaptation_sets.iter().enumerate() {
        for (representation_index, representation) in adaptation.representations.iter().enumerate()
        {
            match lane_contract(representation) {
                Ok(contract) => first_contracts
                    .entry(contract)
                    .or_default()
                    .push((adaptation_index, representation_index)),
                Err(()) => rejections.push(rejection(
                    DashRepresentationLaneRejectionReason::UnsupportedMetadata,
                    0,
                )),
            }
        }
    }
    for (period_ordinal, period) in request.presentation.periods.iter().enumerate().skip(1) {
        for representation in period
            .adaptation_sets
            .iter()
            .flat_map(|adaptation| adaptation.representations.iter())
        {
            if lane_contract(representation).is_err() {
                rejections.push(rejection(
                    DashRepresentationLaneRejectionReason::UnsupportedMetadata,
                    period_ordinal,
                ));
            }
        }
    }

    let mut lanes = Vec::new();
    for (contract, first_locations) in first_contracts {
        if first_locations.len() != 1 {
            rejections.push(rejection(
                DashRepresentationLaneRejectionReason::AmbiguousRequiredPeriod,
                0,
            ));
            continue;
        }
        let mut locations = vec![first_locations[0]];
        let mut complete = true;
        for (period_ordinal, period) in request.presentation.periods.iter().enumerate().skip(1) {
            let matches = period
                .adaptation_sets
                .iter()
                .enumerate()
                .flat_map(|(adaptation_index, adaptation)| {
                    adaptation.representations.iter().enumerate().filter_map({
                        let contract = &contract;
                        move |(representation_index, representation)| {
                            (lane_contract(representation).ok().as_ref() == Some(contract))
                                .then_some((adaptation_index, representation_index))
                        }
                    })
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [location] => locations.push(*location),
                [] => {
                    rejections.push(rejection(
                        DashRepresentationLaneRejectionReason::MissingRequiredPeriod,
                        period_ordinal,
                    ));
                    complete = false;
                    break;
                }
                _ => {
                    rejections.push(rejection(
                        DashRepresentationLaneRejectionReason::AmbiguousRequiredPeriod,
                        period_ordinal,
                    ));
                    complete = false;
                    break;
                }
            }
        }
        if complete {
            let Some(semantic_key) = semantic_key(&contract) else {
                rejections.push(rejection(
                    DashRepresentationLaneRejectionReason::UnsupportedMetadata,
                    0,
                ));
                continue;
            };
            lanes.push(LogicalLane {
                lane: DashLogicalRepresentationLane {
                    semantic_key,
                    locations: locations.into_boxed_slice(),
                    contract: contract.clone(),
                },
                contract,
            });
        }
    }
    if lanes.is_empty() {
        return Err(DashRepresentationLaneCatalogBuildError::NoSelectableLane);
    }
    lanes.sort_by(|left, right| left.lane.semantic_key.cmp(&right.lane.semantic_key));
    if lanes.len() > request.catalog_limit.maximum_entries() {
        return Err(DashRepresentationLaneCatalogBuildError::Catalog(
            ComponentVariantError::CatalogLimitExceeded {
                provided_entries: lanes.len(),
                maximum_entries: request.catalog_limit.maximum_entries(),
            },
        ));
    }

    let provider_default_keys = match request.provider_default {
        DashRepresentationLaneProviderDefault::ExactEvidence(provider_default) => {
            provider_default_lane_keys(&lanes, request.presentation, provider_default)?
        }
        DashRepresentationLaneProviderDefault::NativePreferredHeight(_) => Vec::new(),
    };
    let mut proven_lanes = Vec::with_capacity(lanes.len());
    for (index, logical) in lanes.into_iter().enumerate() {
        let probe = DashRepresentationLaneProbe {
            lane: DashRepresentationLaneProbeId(index.saturating_add(1) as u64),
            kind: logical.contract.kind,
            logical_lane: logical.lane.clone(),
            contract: logical.contract.clone(),
        };
        if prove_manifest_lane(
            request.presentation,
            request.manifest_base,
            &logical.lane.locations,
            request.maximum_planned_segments,
            request.timeline_mode,
            logical.contract.kind,
        )
        .is_err()
        {
            if provider_default_keys.contains(&logical.lane.semantic_key) {
                return Err(
                    DashRepresentationLaneCatalogBuildError::ProviderDefaultTimelineIncompatible,
                );
            }
            rejections.push(rejection(
                DashRepresentationLaneRejectionReason::TimelineIncompatible,
                0,
            ));
            continue;
        }
        let proof = match proof_port.prove_lane(probe) {
            Ok(proof) => proof,
            Err(DashRepresentationLaneProbeError::Cancelled) => {
                return Err(DashRepresentationLaneCatalogBuildError::Cancelled);
            }
            Err(DashRepresentationLaneProbeError::StaleGeneration) => {
                return Err(DashRepresentationLaneCatalogBuildError::StaleGeneration);
            }
            Err(error) => {
                if provider_default_keys.contains(&logical.lane.semantic_key) {
                    return Err(
                        DashRepresentationLaneCatalogBuildError::ProviderDefaultRejected(error),
                    );
                }
                rejections.push(rejection(probe_rejection(error), 0));
                continue;
            }
        };
        if !proof_matches_contract(&proof, &logical.contract) {
            let error = DashRepresentationLaneProbeError::ManifestEvidenceConflict;
            if provider_default_keys.contains(&logical.lane.semantic_key) {
                return Err(
                    DashRepresentationLaneCatalogBuildError::ProviderDefaultRejected(error),
                );
            }
            rejections.push(rejection(
                DashRepresentationLaneRejectionReason::ManifestEvidenceConflict,
                0,
            ));
            continue;
        }
        proven_lanes.push(ProvenLane { logical, proof });
    }
    if proven_lanes.is_empty() {
        return Err(DashRepresentationLaneCatalogBuildError::NoSelectableLane);
    }

    let video_count = proven_lanes
        .iter()
        .filter(|lane| lane.logical.contract.kind == DashMediaKind::Video)
        .count();
    let audio_count = proven_lanes
        .iter()
        .filter(|lane| lane.logical.contract.kind == DashMediaKind::Audio)
        .count();
    let potential_edges = video_count
        .checked_mul(audio_count)
        .ok_or(DashRepresentationLaneCatalogBuildError::CompatibilityBudget)?;
    if potential_edges > request.compatibility_edge_limit.maximum_edges() {
        return Err(DashRepresentationLaneCatalogBuildError::CompatibilityBudget);
    }

    let mut video = Vec::new();
    let mut audio = Vec::new();
    let mut coupled = Vec::new();
    let mut published = Vec::new();
    for proven in proven_lanes {
        let logical = proven.logical;
        let exact_key = ComponentVariantExactKey::new(logical.lane.semantic_key.clone())?;
        let semantic_key = ComponentVariantSemanticKey::new(logical.lane.semantic_key.clone())?;
        match logical.contract.kind {
            DashMediaKind::Video => {
                let exact = ComponentVariantExactIdentity::new(
                    request.catalog_identity.clone(),
                    ComponentKind::Video,
                    exact_key,
                );
                let semantic = ComponentVariantSemanticIdentity::new(
                    request.parent_semantic.clone(),
                    ComponentKind::Video,
                    semantic_key,
                );
                let DashRepresentationLaneProof::VideoOnly(descriptor) = proven.proof else {
                    unreachable!("proof shape validated before publication")
                };
                video.push(VideoComponentVariant::new(
                    exact.clone(),
                    semantic,
                    descriptor,
                ));
                published.push(PublishedLane {
                    kind: DashMediaKind::Video,
                    lane: logical.lane,
                    component_exact: Some(exact),
                    coupled_exact: None,
                });
            }
            DashMediaKind::Audio => {
                let exact = ComponentVariantExactIdentity::new(
                    request.catalog_identity.clone(),
                    ComponentKind::Audio,
                    exact_key,
                );
                let semantic = ComponentVariantSemanticIdentity::new(
                    request.parent_semantic.clone(),
                    ComponentKind::Audio,
                    semantic_key,
                );
                let DashRepresentationLaneProof::AudioOnly(descriptor) = proven.proof else {
                    unreachable!("proof shape validated before publication")
                };
                audio.push(AudioComponentVariant::new(
                    exact.clone(),
                    semantic,
                    descriptor,
                ));
                published.push(PublishedLane {
                    kind: DashMediaKind::Audio,
                    lane: logical.lane,
                    component_exact: Some(exact),
                    coupled_exact: None,
                });
            }
            DashMediaKind::Muxed => {
                let exact =
                    CoupledVariantExactIdentity::new(request.catalog_identity.clone(), exact_key);
                let semantic = CoupledVariantSemanticIdentity::new(
                    request.parent_semantic.clone(),
                    semantic_key,
                );
                let DashRepresentationLaneProof::Muxed { video, audio } = proven.proof else {
                    unreachable!("proof shape validated before publication")
                };
                coupled.push(CoupledComponentVariant::new(
                    exact.clone(),
                    semantic,
                    video,
                    audio,
                ));
                published.push(PublishedLane {
                    kind: DashMediaKind::Muxed,
                    lane: logical.lane,
                    component_exact: None,
                    coupled_exact: Some(exact),
                });
            }
        }
    }

    let video_rows = published
        .iter()
        .filter(|row| row.kind == DashMediaKind::Video)
        .collect::<Vec<_>>();
    let audio_rows = published
        .iter()
        .filter(|row| row.kind == DashMediaKind::Audio)
        .collect::<Vec<_>>();
    let mut edges = Vec::new();
    for video_row in &video_rows {
        for audio_row in &audio_rows {
            if prove_manifest_lane_alignment(
                request.presentation,
                request.manifest_base,
                &video_row.lane.locations,
                &audio_row.lane.locations,
                request.maximum_planned_segments,
                request.timeline_mode,
            )
            .is_ok()
            {
                edges.push(ComponentVariantCompatibilityEdge::new(
                    video_row
                        .component_exact
                        .as_ref()
                        .expect("video row invariant")
                        .clone(),
                    audio_row
                        .component_exact
                        .as_ref()
                        .expect("audio row invariant")
                        .clone(),
                ));
            }
        }
    }
    let video_only = video_rows
        .iter()
        .map(|row| {
            row.component_exact
                .as_ref()
                .expect("video invariant")
                .clone()
        })
        .collect();
    let audio_only = audio_rows
        .iter()
        .map(|row| {
            row.component_exact
                .as_ref()
                .expect("audio invariant")
                .clone()
        })
        .collect();
    let catalog = ComponentVariantCatalog::new(
        request.catalog_identity,
        request.catalog_limit,
        ComponentVariantCatalogEntries::Topology {
            video,
            audio,
            compatibility: ComponentVariantCompatibilityEntries::Sparse {
                edge_limit: request.compatibility_edge_limit,
                edges,
            },
            coupled,
            video_only,
            audio_only,
        },
    )?;
    let provider_default = match request.provider_default {
        DashRepresentationLaneProviderDefault::ExactEvidence(provider_default) => {
            provider_default_selection(
                &catalog,
                &published,
                request.presentation,
                provider_default,
            )?
        }
        DashRepresentationLaneProviderDefault::NativePreferredHeight(preferred_height) => {
            native_provider_default_selection(&catalog, preferred_height)?
        }
    };
    Ok(DashRepresentationLaneCatalog {
        catalog,
        provider_default,
        rejections: rejections.into_boxed_slice(),
        runtime_rows: published.into_boxed_slice(),
    })
}
