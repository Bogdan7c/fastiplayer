//! Синхронная проекция полного declared yt-dlp inventory в зависимый picker.

use anyhow::{Context, Result};
use service_ytdlp::{YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpComposedSelection};
use web_media_core::{ExactSelectionIdentity, StreamLayout, WebMediaSelection};
use web_media_playback_plan::{
    OpaqueAlternativeRank, PlanningCandidateSnapshot, PlaybackCapabilitySnapshot,
    PlaybackSelectionPolicy,
};

use crate::web_media_catalog::{
    WebMediaCatalogAttachment, WebMediaCatalogChoice, WebMediaMode, WebMediaSelectionTarget,
};
use crate::web_media_stream_model::ExtractorCatalogSelectionRoute;

pub(super) struct CatalogAttachmentProjection {
    pub(super) attachment: WebMediaCatalogAttachment,
    pub(super) routes: Vec<ExtractorCatalogSelectionRoute>,
}

struct ProjectedCatalogChoice {
    choice: WebMediaCatalogChoice,
    route: ExtractorCatalogSelectionRoute,
}

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
) -> Result<CatalogAttachmentProjection> {
    let parent = ExactSelectionIdentity::new(
        request.active_selection.exact_identity().clone(),
        request.active_selection.semantic_identity().clone(),
    )
    .context("catalog attachment parent identity is invalid")?;
    let active = match request.active_composed {
        Some(selection) => separate_components_target(selection, request.active_selection)?,
        None => candidate_target(request.active_selection)?,
    };
    let projected_choices = complete_projected_choices(
        parent_choices(
            request.candidate_snapshot,
            request.planning_snapshot,
            request.capabilities,
            request.policy,
        )?,
        &active,
    )?;
    let mut choices = Vec::with_capacity(projected_choices.len());
    let mut routes = Vec::with_capacity(projected_choices.len());
    for projected in projected_choices {
        choices.push(projected.choice);
        routes.push(projected.route);
    }
    Ok(CatalogAttachmentProjection {
        attachment: WebMediaCatalogAttachment::new(parent, choices, active)?,
        routes,
    })
}

fn candidate_target(selection: &YtDlpCandidateSelection) -> Result<WebMediaSelectionTarget> {
    let exact = ExactSelectionIdentity::new(
        selection.exact_identity().clone(),
        selection.semantic_identity().clone(),
    )
    .context("candidate catalog identity is invalid")?;
    Ok(WebMediaSelectionTarget::Candidate {
        selection: Box::new(WebMediaSelection::candidate(exact)),
    })
}

fn separate_components_target(
    selection: &YtDlpComposedSelection,
    parent_preference: &YtDlpCandidateSelection,
) -> Result<WebMediaSelectionTarget> {
    let target = WebMediaSelection::candidate(
        ExactSelectionIdentity::new(
            selection.descriptor().identity().clone(),
            selection.descriptor().semantic_identity().clone(),
        )
        .context("separate-components catalog identity is invalid")?,
    );
    let parent = WebMediaSelection::candidate(
        ExactSelectionIdentity::new(
            parent_preference.exact_identity().clone(),
            parent_preference.semantic_identity().clone(),
        )
        .context("separate-components parent identity is invalid")?,
    );
    Ok(WebMediaSelectionTarget::SeparateComponents {
        selection: Box::new(target),
        parent_preference: Box::new(parent),
    })
}

/// Возвращает число projected parent choices для cross-module regression tests.
#[cfg(test)]
pub(super) fn projected_parent_choice_count(
    request: CatalogAttachmentRequest<'_>,
) -> Result<usize> {
    parent_choices(
        request.candidate_snapshot,
        request.planning_snapshot,
        request.capabilities,
        request.policy,
    )
    .map(|choices| choices.len())
}

fn complete_projected_choices(
    mut choices: Vec<ProjectedCatalogChoice>,
    active: &WebMediaSelectionTarget,
) -> Result<Vec<ProjectedCatalogChoice>> {
    choices.sort_by_key(|projected| (projected.choice.rank, projected.choice.mode));
    if choices.windows(2).any(|pair| {
        pair[0].choice.rank == pair[1].choice.rank
            && pair[0].choice.mode == pair[1].choice.mode
            && pair[0].choice.target != pair[1].choice.target
    }) {
        anyhow::bail!("parent catalog ranking неоднозначен без source-order tie-breaker");
    }
    if !choices
        .iter()
        .any(|projected| &projected.choice.target == active)
    {
        anyhow::bail!("active Installed choice отсутствует в playable parent catalog");
    }
    Ok(choices)
}

/// Стабилизирует pure neutral rows; отдельно оставлен как focused projection boundary.
#[cfg(test)]
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
) -> Result<Vec<ProjectedCatalogChoice>> {
    snapshot
        .validate_planning_snapshot_alignment(planning)
        .context("Catalog service/planner snapshots не соответствуют друг другу")?;
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
        let target = candidate_target(&selection)?;
        choices.push(ProjectedCatalogChoice {
            choice: WebMediaCatalogChoice {
                mode,
                video,
                rank: OpaqueAlternativeRank::parent(parent_rank),
                target: target.clone(),
            },
            route: ExtractorCatalogSelectionRoute::Candidate {
                target,
                selection: Box::new(selection),
            },
        });
    }
    let current_audio = None;
    let mut playable_audio = snapshot
        .accepted_candidates()
        .filter(|candidate| {
            matches!(candidate.descriptor().layout(), StreamLayout::AudioOnly(_))
                && !rejected.contains(candidate.descriptor().identity())
        })
        .collect::<Vec<_>>();
    playable_audio.sort_by(|left, right| {
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
    });
    for video in snapshot.accepted_candidates().filter(|candidate| {
        matches!(candidate.descriptor().layout(), StreamLayout::VideoOnly(_))
            && !rejected.contains(candidate.descriptor().identity())
    }) {
        let video_selection = snapshot.selection_for(video)?;
        let parent_rank = ranking
            .rank_of_candidate(
                video_selection.exact_identity(),
                video_selection.semantic_identity(),
            )
            .ok_or_else(|| {
                anyhow::anyhow!("composed video отсутствует в opaque planner ranking")
            })?;
        let composed = first_compatible_composition(&playable_audio, |audio| {
            let audio_selection = snapshot.selection_for(audio)?;
            compose_catalog_inventory_av(snapshot, &video_selection, &audio_selection)
        })?;
        let Some(composed) = composed else {
            continue;
        };
        let StreamLayout::Separate {
            video: component, ..
        } = composed.descriptor().layout()
        else {
            continue;
        };
        let target = separate_components_target(&composed, &video_selection)?;
        choices.push(ProjectedCatalogChoice {
            choice: WebMediaCatalogChoice {
                mode: WebMediaMode::VideoAndAudio,
                video: Some(component.video().clone()),
                rank: OpaqueAlternativeRank::parent(parent_rank),
                target: target.clone(),
            },
            route: ExtractorCatalogSelectionRoute::SeparateComponents {
                target,
                selection: Box::new(composed),
                parent_preference: Box::new(video_selection),
            },
        });
    }
    Ok(choices)
}

/// Возвращает максимум одну совместимую композицию для одной video-строки каталога.
///
/// Такой boundary не позволяет случайно превратить catalog projection в декартово
/// произведение `video × audio`: следующие audio-кандидаты после первого успеха
/// вообще не рассматриваются.
fn first_compatible_composition<T, U>(
    audio_candidates: &[T],
    mut compose: impl FnMut(&T) -> Result<Option<U>>,
) -> Result<Option<U>> {
    for audio_candidate in audio_candidates {
        if let Some(composition) = compose(audio_candidate)? {
            return Ok(Some(composition));
        }
    }
    Ok(None)
}

/// Отделяет optional non-inventory alternative от настоящего composition error-а.
fn compose_catalog_inventory_av(
    snapshot: &YtDlpCandidateSnapshot,
    video: &YtDlpCandidateSelection,
    audio: &YtDlpCandidateSelection,
) -> Result<Option<YtDlpComposedSelection>> {
    match snapshot.compose_inventory_av(video, audio) {
        Ok(composed) => Ok(Some(composed)),
        Err(service_ytdlp::YtDlpCompositionError::ForeignGenerationOrInventory) => Ok(None),
        Err(error) => Err(error.into()),
    }
}

fn layout_facets(
    layout: &StreamLayout,
) -> (WebMediaMode, Option<web_media_core::VideoTrackDescriptor>) {
    match layout {
        StreamLayout::Muxed(component) => {
            (WebMediaMode::VideoAndAudio, Some(component.video().clone()))
        }
        // Absent codec — только picker projection: height/fps/HDR остаются, codec = «Авто».
        StreamLayout::HlsMuxedCodecDeferred(component) => (
            WebMediaMode::VideoAndAudio,
            Some(web_media_core::VideoTrackDescriptor::new(
                web_media_core::NormalizedCodec::parse(
                    web_media_core::RawCodecIdentity::new("none")
                        .expect("literal none codec identity"),
                ),
                component.width(),
                Some(component.height()),
                component.frame_rate(),
                component.bitrate(),
                component.dynamic_range(),
            )),
        ),
        StreamLayout::ContentProbed(component) => {
            let mode = match (component.video(), component.audio()) {
                (web_media_core::ContentProbedTrackEvidence::Absent, _) => WebMediaMode::AudioOnly,
                (_, web_media_core::ContentProbedTrackEvidence::Absent) => WebMediaMode::VideoOnly,
                (
                    web_media_core::ContentProbedTrackEvidence::Declared(_),
                    web_media_core::ContentProbedTrackEvidence::Declared(_),
                ) => WebMediaMode::VideoAndAudio,
                _ => WebMediaMode::Automatic,
            };
            (mode, component.video().declared().cloned())
        }
        StreamLayout::Separate { video, .. } => {
            (WebMediaMode::VideoAndAudio, Some(video.video().clone()))
        }
        StreamLayout::VideoOnly(video) => (WebMediaMode::VideoOnly, Some(video.video().clone())),
        StreamLayout::AudioOnly(_) => (WebMediaMode::AudioOnly, None),
    }
}

#[cfg(test)]
mod tests;
