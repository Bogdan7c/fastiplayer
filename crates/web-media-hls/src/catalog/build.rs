use std::collections::HashMap;

use hls_playlist_core::{ExactReference, MasterPlaylist, MediaRenditionType};
use web_media_core::{
    AudioComponentVariant, ComponentVariantCatalog, ComponentVariantCatalogEntries,
    ComponentVariantCompatibilityEdge, ComponentVariantCompatibilityEntries,
    ComponentVariantSelectionRequest, CoupledComponentVariant, VideoComponentVariant,
};

use super::reopen::{
    HlsCatalogRuntimeAudioRow, HlsCatalogRuntimeAudioSource, HlsCatalogRuntimeCoupledRow,
    HlsCatalogRuntimeMap, HlsCatalogRuntimeVideoRow,
};
use super::rows::{
    build_coupled_row, build_rendition_audio_row, build_variant_audio_row, build_video_row,
};
use super::*;
use crate::HlsMainTrackLayoutIntent;
use crate::open::{select_master, select_master_at_index};

#[derive(Clone, Copy)]
enum ChildUse {
    Variant(usize),
    AlternateAudio(usize),
}

struct UniqueChild<'master> {
    id: HlsCatalogChildId,
    reference: &'master ExactReference,
    uses: Vec<ChildUse>,
    proof: Option<HlsCatalogChildProof>,
}

struct PendingVideo {
    variant_index: usize,
    child: HlsCatalogChildId,
    semantic_key: String,
    variant: VideoComponentVariant,
    alignment: HlsCatalogAlignmentProof,
    container: HlsRequiredContainer,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AudioOrigin {
    Variant(usize),
    Rendition(usize),
}

struct PendingAudio {
    origin: AudioOrigin,
    child: HlsCatalogChildId,
    semantic_key: String,
    variant: AudioComponentVariant,
    alignment: HlsCatalogAlignmentProof,
    container: HlsRequiredContainer,
}

struct PendingCoupled {
    variant_index: usize,
    child: HlsCatalogChildId,
    semantic_key: String,
    variant: CoupledComponentVariant,
    container: HlsRequiredContainer,
}

/// Строит immutable topology snapshot и вызывает proof один раз на exact child reference.
pub fn build_hls_catalog(
    request: HlsCatalogBuildRequest<'_>,
    proof_port: &mut dyn HlsCatalogChildProofPort,
) -> Result<HlsCatalogSnapshot, HlsCatalogBuildError> {
    let selected = match request.provider_default_variant_index {
        Some(variant_index) => {
            select_master_at_index(request.master, variant_index, request.provider_default)
        }
        None => select_master(request.master, request.provider_default),
    }
    .map_err(HlsCatalogBuildError::ProviderDefaultSelection)?;
    let selected_variant_index = request
        .master
        .variants
        .iter()
        .position(|variant| variant == &selected.variant)
        .ok_or(HlsCatalogBuildError::SemanticIdentity)?;
    let selected_audio_index = selected.audio.as_ref().and_then(|selected_audio| {
        request
            .master
            .renditions
            .iter()
            .position(|rendition| rendition == selected_audio)
    });

    let mut children = collect_unique_children(request.master);
    if children.len() > request.policy.maximum_unique_children.get() {
        return Err(HlsCatalogBuildError::UniqueChildLimitExceeded {
            provided: children.len(),
            maximum: request.policy.maximum_unique_children.get(),
        });
    }
    let selected_children = selected_child_ids(
        &children,
        selected_variant_index,
        selected_audio_index,
        request.policy.provider_default_audio,
    );
    let mut rejections = Vec::new();
    for child in &mut children {
        match proof_port.prove_child(HlsCatalogChildProbe {
            child: child.id,
            role: child_role(&child.uses),
            reference: child.reference.clone(),
        }) {
            Ok(proof) => child.proof = Some(proof),
            Err(HlsCatalogChildProofError::Cancelled) => {
                return Err(HlsCatalogBuildError::Cancelled);
            }
            Err(HlsCatalogChildProofError::StaleGeneration) => {
                return Err(HlsCatalogBuildError::StaleGeneration);
            }
            Err(HlsCatalogChildProofError::Rejected(reason)) => {
                reject_child(child.id, reason, &selected_children, &mut rejections)?;
            }
        }
    }

    let mut videos = Vec::new();
    let mut audios = Vec::new();
    let mut coupled = Vec::new();
    for (variant_index, variant) in request.master.variants.iter().enumerate() {
        let child = child_for_variant(&children, variant_index);
        let Some(proof) = child.proof.as_ref() else {
            continue;
        };
        match &proof.tracks {
            HlsCatalogTrackProof::VideoOnly(video) => {
                match build_video_row(&request.catalog_identity, variant, proof.container, video) {
                    Ok((semantic_key, video)) => videos.push(PendingVideo {
                        variant_index,
                        child: child.id,
                        semantic_key,
                        variant: video,
                        alignment: proof.alignment,
                        container: proof.container,
                    }),
                    Err(reason) => {
                        reject_child(child.id, reason, &selected_children, &mut rejections)?;
                    }
                }
            }
            HlsCatalogTrackProof::AudioOnly(audio) => {
                match build_variant_audio_row(
                    &request.catalog_identity,
                    variant,
                    proof.container,
                    audio,
                ) {
                    Ok((semantic_key, audio)) => audios.push(PendingAudio {
                        origin: AudioOrigin::Variant(variant_index),
                        child: child.id,
                        semantic_key,
                        variant: audio,
                        alignment: proof.alignment,
                        container: proof.container,
                    }),
                    Err(reason) => {
                        reject_child(child.id, reason, &selected_children, &mut rejections)?;
                    }
                }
            }
            HlsCatalogTrackProof::Muxed { video, audio } => {
                match build_coupled_row(
                    &request.catalog_identity,
                    variant,
                    proof.container,
                    video,
                    audio,
                ) {
                    Ok((semantic_key, presentation)) => coupled.push(PendingCoupled {
                        variant_index,
                        child: child.id,
                        semantic_key,
                        variant: presentation,
                        container: proof.container,
                    }),
                    Err(reason) => {
                        reject_child(child.id, reason, &selected_children, &mut rejections)?;
                    }
                }
            }
        }
    }

    for (rendition_index, rendition) in request.master.renditions.iter().enumerate() {
        if rendition.rendition_type != MediaRenditionType::Audio || rendition.uri.is_none() {
            continue;
        }
        let child = child_for_audio(&children, rendition_index);
        let Some(proof) = child.proof.as_ref() else {
            continue;
        };
        let HlsCatalogTrackProof::AudioOnly(audio) = &proof.tracks else {
            reject_child(
                child.id,
                HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
                &selected_children,
                &mut rejections,
            )?;
            continue;
        };
        match build_rendition_audio_row(
            &request.catalog_identity,
            rendition,
            proof.container,
            audio,
        ) {
            Ok((semantic_key, audio)) => audios.push(PendingAudio {
                origin: AudioOrigin::Rendition(rendition_index),
                child: child.id,
                semantic_key,
                variant: audio,
                alignment: proof.alignment,
                container: proof.container,
            }),
            Err(reason) => {
                reject_child(child.id, reason, &selected_children, &mut rejections)?;
            }
        }
    }

    isolate_ambiguous_rows(
        &mut videos,
        &mut audios,
        &mut coupled,
        &selected_children,
        &mut rejections,
    )?;
    sort_rows(&mut videos, &mut audios, &mut coupled);

    let video_only = videos
        .iter()
        .map(|row| row.variant.exact_identity().clone())
        .collect::<Vec<_>>();
    let audio_only = audios
        .iter()
        .map(|row| row.variant.exact_identity().clone())
        .collect::<Vec<_>>();
    let edges = compatibility_edges(request.master, &videos, &audios);
    let compatibility = if edges.is_empty() {
        ComponentVariantCompatibilityEntries::Unavailable
    } else {
        ComponentVariantCompatibilityEntries::Sparse {
            edge_limit: request.policy.compatibility_edge_limit,
            edges,
        }
    };

    if videos.is_empty() && audios.is_empty() && coupled.is_empty() {
        return Err(HlsCatalogBuildError::NoSelectableRows);
    }
    let default_request = provider_default_request(
        request.provider_default,
        selected_variant_index,
        selected_audio_index,
        request.policy.provider_default_audio,
        &videos,
        &audios,
        &coupled,
    )?;
    let runtime = runtime_map(request.master, &videos, &audios, &coupled);
    let catalog = ComponentVariantCatalog::new(
        request.catalog_identity,
        request.policy.catalog_limit,
        ComponentVariantCatalogEntries::Topology {
            video: videos.into_iter().map(|row| row.variant).collect(),
            audio: audios.into_iter().map(|row| row.variant).collect(),
            compatibility,
            coupled: coupled.into_iter().map(|row| row.variant).collect(),
            video_only,
            audio_only,
        },
    )?;
    let provider_default = catalog.select_exact(default_request).map_err(|error| {
        if error == ComponentVariantError::IncompatibleComponentPair {
            HlsCatalogBuildError::ProviderDefaultRejected {
                reason: HlsCatalogSiblingRejectionReason::ManifestEvidenceConflict,
            }
        } else {
            HlsCatalogBuildError::Catalog(error)
        }
    })?;
    Ok(HlsCatalogSnapshot {
        catalog,
        provider_default,
        sibling_rejections: rejections.into_boxed_slice(),
        runtime,
    })
}

fn runtime_map(
    master: &MasterPlaylist,
    videos: &[PendingVideo],
    audios: &[PendingAudio],
    coupled: &[PendingCoupled],
) -> HlsCatalogRuntimeMap {
    HlsCatalogRuntimeMap {
        videos: videos
            .iter()
            .map(|row| HlsCatalogRuntimeVideoRow {
                identity: row.variant.exact_identity().clone(),
                variant: master.variants[row.variant_index].clone(),
                container: row.container,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        audios: audios
            .iter()
            .map(|row| HlsCatalogRuntimeAudioRow {
                identity: row.variant.exact_identity().clone(),
                source: match row.origin {
                    AudioOrigin::Variant(index) => {
                        HlsCatalogRuntimeAudioSource::Variant(master.variants[index].clone())
                    }
                    AudioOrigin::Rendition(index) => {
                        HlsCatalogRuntimeAudioSource::Rendition(master.renditions[index].clone())
                    }
                },
                container: row.container,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        coupled: coupled
            .iter()
            .map(|row| HlsCatalogRuntimeCoupledRow {
                identity: row.variant.exact_identity().clone(),
                variant: master.variants[row.variant_index].clone(),
                container: row.container,
            })
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    }
}

fn collect_unique_children(master: &MasterPlaylist) -> Vec<UniqueChild<'_>> {
    let mut children = Vec::<UniqueChild<'_>>::new();
    for (index, variant) in master.variants.iter().enumerate() {
        add_child(&mut children, &variant.uri, ChildUse::Variant(index));
    }
    for (index, rendition) in master.renditions.iter().enumerate() {
        if rendition.rendition_type == MediaRenditionType::Audio
            && let Some(reference) = rendition.uri.as_ref()
        {
            add_child(&mut children, reference, ChildUse::AlternateAudio(index));
        }
    }
    children
}

fn add_child<'master>(
    children: &mut Vec<UniqueChild<'master>>,
    reference: &'master ExactReference,
    child_use: ChildUse,
) {
    if let Some(existing) = children
        .iter_mut()
        .find(|child| child.reference == reference)
    {
        existing.uses.push(child_use);
        return;
    }
    let id = HlsCatalogChildId::from_index(children.len());
    children.push(UniqueChild {
        id,
        reference,
        uses: vec![child_use],
        proof: None,
    });
}

fn child_role(uses: &[ChildUse]) -> HlsCatalogChildRole {
    let has_variant = uses.iter().any(|role| matches!(role, ChildUse::Variant(_)));
    let has_audio = uses
        .iter()
        .any(|role| matches!(role, ChildUse::AlternateAudio(_)));
    match (has_variant, has_audio) {
        (true, true) => HlsCatalogChildRole::Shared,
        (true, false) => HlsCatalogChildRole::Variant,
        (false, true) => HlsCatalogChildRole::AlternateAudio,
        (false, false) => unreachable!("every child has at least one use"),
    }
}

fn selected_child_ids(
    children: &[UniqueChild<'_>],
    variant_index: usize,
    audio_index: Option<usize>,
    audio_policy: HlsProviderDefaultAudioPolicy,
) -> Vec<HlsCatalogChildId> {
    children
        .iter()
        .filter(|child| {
            child.uses.iter().any(|child_use| match child_use {
                ChildUse::Variant(index) => *index == variant_index,
                ChildUse::AlternateAudio(index) => {
                    audio_policy == HlsProviderDefaultAudioPolicy::RequireDeclared
                        && audio_index == Some(*index)
                }
            })
        })
        .map(|child| child.id)
        .collect()
}

fn child_for_variant<'a>(children: &'a [UniqueChild<'a>], index: usize) -> &'a UniqueChild<'a> {
    children
        .iter()
        .find(|child| {
            child
                .uses
                .iter()
                .any(|role| matches!(role, ChildUse::Variant(candidate) if *candidate == index))
        })
        .expect("variant child was collected")
}

fn child_for_audio<'a>(children: &'a [UniqueChild<'a>], index: usize) -> &'a UniqueChild<'a> {
    children
        .iter()
        .find(|child| {
            child.uses.iter().any(
                |role| matches!(role, ChildUse::AlternateAudio(candidate) if *candidate == index),
            )
        })
        .expect("audio child was collected")
}

fn reject_child(
    child: HlsCatalogChildId,
    reason: HlsCatalogSiblingRejectionReason,
    selected_children: &[HlsCatalogChildId],
    rejections: &mut Vec<HlsCatalogSiblingRejection>,
) -> Result<(), HlsCatalogBuildError> {
    if selected_children.contains(&child) {
        return Err(HlsCatalogBuildError::ProviderDefaultRejected { reason });
    }
    if !rejections.iter().any(|rejection| rejection.child == child) {
        rejections.push(HlsCatalogSiblingRejection { child, reason });
    }
    Ok(())
}

fn isolate_ambiguous_rows(
    videos: &mut Vec<PendingVideo>,
    audios: &mut Vec<PendingAudio>,
    coupled: &mut Vec<PendingCoupled>,
    selected_children: &[HlsCatalogChildId],
    rejections: &mut Vec<HlsCatalogSiblingRejection>,
) -> Result<(), HlsCatalogBuildError> {
    let duplicate_video = duplicate_keys(videos.iter().map(|row| row.semantic_key.as_str()));
    let duplicate_audio = duplicate_keys(audios.iter().map(|row| row.semantic_key.as_str()));
    let duplicate_coupled = duplicate_keys(coupled.iter().map(|row| row.semantic_key.as_str()));
    remove_ambiguous(
        videos,
        &duplicate_video,
        selected_children,
        rejections,
        |row| row.child,
    )?;
    remove_ambiguous(
        audios,
        &duplicate_audio,
        selected_children,
        rejections,
        |row| row.child,
    )?;
    remove_ambiguous(
        coupled,
        &duplicate_coupled,
        selected_children,
        rejections,
        |row| row.child,
    )?;
    Ok(())
}

fn duplicate_keys<'a>(keys: impl Iterator<Item = &'a str>) -> Vec<bool> {
    let keys = keys.collect::<Vec<_>>();
    let mut counts = HashMap::new();
    for key in &keys {
        *counts.entry(*key).or_insert(0_usize) += 1;
    }
    keys.iter().map(|key| counts[key] > 1).collect()
}

fn remove_ambiguous<T>(
    rows: &mut Vec<T>,
    duplicate: &[bool],
    selected_children: &[HlsCatalogChildId],
    rejections: &mut Vec<HlsCatalogSiblingRejection>,
    child: impl Fn(&T) -> HlsCatalogChildId,
) -> Result<(), HlsCatalogBuildError> {
    for (row, duplicate) in rows.iter().zip(duplicate) {
        if *duplicate {
            reject_child(
                child(row),
                HlsCatalogSiblingRejectionReason::AmbiguousSemanticIdentity,
                selected_children,
                rejections,
            )?;
        }
    }
    let mut index = 0_usize;
    rows.retain(|_| {
        let retain = !duplicate[index];
        index += 1;
        retain
    });
    Ok(())
}

fn sort_rows(
    videos: &mut [PendingVideo],
    audios: &mut [PendingAudio],
    coupled: &mut [PendingCoupled],
) {
    videos.sort_by(|left, right| left.semantic_key.cmp(&right.semantic_key));
    audios.sort_by(|left, right| left.semantic_key.cmp(&right.semantic_key));
    coupled.sort_by(|left, right| left.semantic_key.cmp(&right.semantic_key));
}

fn compatibility_edges(
    master: &MasterPlaylist,
    videos: &[PendingVideo],
    audios: &[PendingAudio],
) -> Vec<ComponentVariantCompatibilityEdge> {
    let mut edges = Vec::new();
    for video in videos {
        let Some(group) = master.variants[video.variant_index].audio_group.as_deref() else {
            continue;
        };
        for audio in audios {
            let AudioOrigin::Rendition(rendition_index) = audio.origin else {
                continue;
            };
            if master.renditions[rendition_index].group_id.as_ref() == group
                && video.alignment == audio.alignment
            {
                edges.push(ComponentVariantCompatibilityEdge::new(
                    video.variant.exact_identity().clone(),
                    audio.variant.exact_identity().clone(),
                ));
            }
        }
    }
    edges
}

fn provider_default_request(
    intent: &crate::HlsVariantSelectionIntent,
    variant_index: usize,
    audio_index: Option<usize>,
    audio_policy: HlsProviderDefaultAudioPolicy,
    videos: &[PendingVideo],
    audios: &[PendingAudio],
    coupled: &[PendingCoupled],
) -> Result<ComponentVariantSelectionRequest, HlsCatalogBuildError> {
    match intent.main_track_layout {
        HlsMainTrackLayoutIntent::MuxedAv => {
            if audio_index.is_some() {
                return Err(HlsCatalogBuildError::ProviderDefaultRejected {
                    reason: HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
                });
            }
            let row = coupled
                .iter()
                .find(|row| row.variant_index == variant_index)
                .ok_or(HlsCatalogBuildError::ProviderDefaultRejected {
                    reason: HlsCatalogSiblingRejectionReason::MissingEmbeddedAudio,
                })?;
            Ok(ComponentVariantSelectionRequest::Coupled {
                presentation: row.variant.exact_identity().clone(),
            })
        }
        HlsMainTrackLayoutIntent::VideoOnly => {
            let video = videos
                .iter()
                .find(|row| row.variant_index == variant_index)
                .ok_or(HlsCatalogBuildError::ProviderDefaultRejected {
                    reason: HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
                })?;
            match audio_index {
                Some(audio_index) => {
                    match audios
                        .iter()
                        .find(|row| row.origin == AudioOrigin::Rendition(audio_index))
                    {
                        Some(audio) => Ok(ComponentVariantSelectionRequest::VideoAndAudio {
                            video: video.variant.exact_identity().clone(),
                            audio: audio.variant.exact_identity().clone(),
                        }),
                        None if audio_policy
                            == HlsProviderDefaultAudioPolicy::AllowUnsupportedOmission =>
                        {
                            Ok(ComponentVariantSelectionRequest::VideoOnly {
                                video: video.variant.exact_identity().clone(),
                            })
                        }
                        None => Err(HlsCatalogBuildError::ProviderDefaultRejected {
                            reason: HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
                        }),
                    }
                }
                None => Ok(ComponentVariantSelectionRequest::VideoOnly {
                    video: video.variant.exact_identity().clone(),
                }),
            }
        }
        HlsMainTrackLayoutIntent::AudioOnly => {
            let audio = audios
                .iter()
                .find(|row| row.origin == AudioOrigin::Variant(variant_index))
                .ok_or(HlsCatalogBuildError::ProviderDefaultRejected {
                    reason: HlsCatalogSiblingRejectionReason::UnsupportedTrackShape,
                })?;
            Ok(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.variant.exact_identity().clone(),
            })
        }
    }
}
