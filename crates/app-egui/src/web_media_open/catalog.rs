//! Синхронная проекция полного declared yt-dlp inventory в зависимый picker.

use anyhow::{Context, Result};
use service_ytdlp::{YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpComposedSelection};
use web_media_core::{ExactSelectionIdentity, StreamLayout};
use web_media_playback_plan::{
    OpaqueAlternativeRank, PlanningCandidateSnapshot, PlaybackCapabilitySnapshot,
    PlaybackSelectionPolicy,
};

use crate::web_media_catalog::{
    WebMediaCatalogAttachment, WebMediaCatalogChoice, WebMediaMode, WebMediaSelectionTarget,
};

pub(super) struct CatalogAttachmentRequest<'a> {
    pub(super) candidate_snapshot: &'a YtDlpCandidateSnapshot,
    pub(super) planning_snapshot: &'a PlanningCandidateSnapshot,
    pub(super) capabilities: PlaybackCapabilitySnapshot<'a>,
    pub(super) policy: &'a PlaybackSelectionPolicy,
    pub(super) active_selection: &'a YtDlpCandidateSelection,
    pub(super) active_composed: Option<&'a YtDlpComposedSelection>,
}

pub(super) fn catalog_attachment(
    request: CatalogAttachmentRequest<'_>,
) -> Result<WebMediaCatalogAttachment> {
    let parent = ExactSelectionIdentity::new(
        request.active_selection.exact_identity().clone(),
        request.active_selection.semantic_identity().clone(),
    )
    .context("catalog attachment parent identity is invalid")?;
    let active = match request.active_composed {
        Some(selection) => WebMediaSelectionTarget::Composed {
            selection: Box::new(selection.clone()),
            parent_preference: Box::new(request.active_selection.clone()),
        },
        None => WebMediaSelectionTarget::Parent {
            selection: Box::new(request.active_selection.clone()),
        },
    };
    let choices = complete_parent_choices(
        parent_choices(
            request.candidate_snapshot,
            request.planning_snapshot,
            request.capabilities,
            request.policy,
        )?,
        &active,
    )?;
    WebMediaCatalogAttachment::new(parent, choices, active)
}

fn complete_parent_choices(
    mut choices: Vec<WebMediaCatalogChoice>,
    active: &WebMediaSelectionTarget,
) -> Result<Vec<WebMediaCatalogChoice>> {
    choices.sort_by_key(|choice| (choice.rank, choice.mode));
    if choices.windows(2).any(|pair| {
        pair[0].rank == pair[1].rank
            && pair[0].mode == pair[1].mode
            && pair[0].target != pair[1].target
    }) {
        anyhow::bail!("parent catalog ranking неоднозначен без source-order tie-breaker");
    }
    if !choices.iter().any(|choice| &choice.target == active) {
        anyhow::bail!("active Installed choice отсутствует в playable parent catalog");
    }
    Ok(choices)
}

fn parent_choices(
    snapshot: &YtDlpCandidateSnapshot,
    planning: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<Vec<WebMediaCatalogChoice>> {
    let ranking =
        web_media_playback_plan::rank_playable_opaque_alternatives(planning, capabilities, policy)?;
    let rejected = ranking
        .rejected_candidates()
        .iter()
        .map(|candidate| candidate.exact_identity())
        .collect::<std::collections::HashSet<_>>();
    let mut choices = Vec::new();
    for candidate in snapshot.accepted_candidates() {
        if rejected.contains(candidate.descriptor().identity()) {
            continue;
        }
        let (mode, video) = layout_facets(candidate.descriptor().layout());
        let selection = snapshot.selection_for(candidate)?;
        let parent_rank = ranking
            .rank_of_candidate(selection.exact_identity(), selection.semantic_identity())
            .ok_or_else(|| {
                anyhow::anyhow!("playable candidate отсутствует в opaque planner ranking")
            })?;
        choices.push(WebMediaCatalogChoice {
            mode,
            video: video.cloned(),
            rank: OpaqueAlternativeRank::parent(parent_rank),
            target: WebMediaSelectionTarget::Parent {
                selection: Box::new(selection),
            },
        });
    }
    let current_audio = None;
    let playable_audio = snapshot
        .accepted_candidates()
        .filter(|candidate| {
            matches!(candidate.descriptor().layout(), StreamLayout::AudioOnly(_))
                && !rejected.contains(candidate.descriptor().identity())
        })
        .collect::<Vec<_>>();
    for video in snapshot.accepted_candidates().filter(|candidate| {
        matches!(candidate.descriptor().layout(), StreamLayout::VideoOnly(_))
            && !rejected.contains(candidate.descriptor().identity())
    }) {
        let Some(audio) = playable_audio.iter().copied().min_by(|left, right| {
            web_media_playback_plan::compare_audio_fallback(
                current_audio,
                left.descriptor().semantic_identity(),
                left.audio_fallback_rank()
                    .expect("audio-only candidate has audio rank"),
                right.descriptor().semantic_identity(),
                right
                    .audio_fallback_rank()
                    .expect("audio-only candidate has audio rank"),
            )
        }) else {
            continue;
        };
        let video_selection = snapshot.selection_for(video)?;
        let audio_selection = snapshot.selection_for(audio)?;
        let parent_rank = ranking
            .rank_of_candidate(
                video_selection.exact_identity(),
                video_selection.semantic_identity(),
            )
            .ok_or_else(|| {
                anyhow::anyhow!("composed video отсутствует в opaque planner ranking")
            })?;
        let composed = snapshot.compose_inventory_av(&video_selection, &audio_selection)?;
        let StreamLayout::Separate {
            video: component, ..
        } = composed.descriptor().layout()
        else {
            continue;
        };
        choices.push(WebMediaCatalogChoice {
            mode: WebMediaMode::VideoAndAudio,
            video: Some(component.video().clone()),
            rank: OpaqueAlternativeRank::parent(parent_rank),
            target: WebMediaSelectionTarget::Composed {
                selection: Box::new(composed),
                parent_preference: Box::new(video_selection),
            },
        });
    }
    Ok(choices)
}

fn layout_facets(
    layout: &StreamLayout,
) -> (WebMediaMode, Option<&web_media_core::VideoTrackDescriptor>) {
    match layout {
        StreamLayout::Muxed(component) => (WebMediaMode::VideoAndAudio, Some(component.video())),
        StreamLayout::Separate { video, .. } => (WebMediaMode::VideoAndAudio, Some(video.video())),
        StreamLayout::VideoOnly(video) => (WebMediaMode::VideoOnly, Some(video.video())),
        StreamLayout::AudioOnly(_) => (WebMediaMode::AudioOnly, None),
    }
}

#[cfg(test)]
mod tests;
