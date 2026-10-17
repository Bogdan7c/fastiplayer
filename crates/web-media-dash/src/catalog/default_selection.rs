//! Provider-default выбирается только из доказанных catalog rows и compatibility edges.

use super::*;
use web_media_core::{AudioComponentVariant, AudioTrackDescriptor, DefaultAudioSelectionTier};

pub(super) fn provider_default_selection(
    catalog: &ComponentVariantCatalog,
    rows: &[PublishedLane],
    presentation: &DashMpd,
    provider_default: &DashPresentationSelection,
) -> Result<ComponentVariantSelection, DashRepresentationLaneCatalogBuildError> {
    match provider_default {
        DashPresentationSelection::Single { main } => {
            let row = unique_default_row(rows, presentation, main)?;
            let request = match &row.exact {
                PublishedLaneExact::Video(video) => ComponentVariantSelectionRequest::VideoOnly {
                    video: video.clone(),
                },
                PublishedLaneExact::Audio(audio) => ComponentVariantSelectionRequest::AudioOnly {
                    audio: audio.clone(),
                },
                PublishedLaneExact::Muxed(presentation) => {
                    ComponentVariantSelectionRequest::Coupled {
                        presentation: presentation.clone(),
                    }
                }
            };
            catalog.select_exact(request).map_err(Into::into)
        }
        DashPresentationSelection::Separate { video, audio } => {
            let video = unique_default_row(rows, presentation, video)?;
            let audio = unique_default_row(rows, presentation, audio)?;
            let (PublishedLaneExact::Video(video), PublishedLaneExact::Audio(audio)) =
                (&video.exact, &audio.exact)
            else {
                return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing);
            };
            catalog
                .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                    video: video.clone(),
                    audio: audio.clone(),
                })
                .map_err(|error| match error {
                    ComponentVariantError::IncompatibleComponentPair => {
                        DashRepresentationLaneCatalogBuildError::ProviderDefaultIncompatible
                    }
                    other => DashRepresentationLaneCatalogBuildError::Catalog(other),
                })
        }
    }
}

/// Выбирает native default только из реально опубликованных selectable relations.
///
/// Приоритет сохраняет полную presentation: proven separate A/V, затем coupled,
/// затем честный video-only и audio-only fallback. Пары никогда не строятся как
/// Cartesian rows: каждая separate selection проверяется catalog compatibility.
///
/// Внутри каждого шага audio с `DefaultAudioSelectionTier::FallbackOnly` (AC-3/E-AC-3)
/// выбирается, только если подходящего `Primary` audio нет: так прежний default
/// (например, AAC) не меняется оттого, что Dolby-rows стали воспроизводимыми.
pub(super) fn native_provider_default_selection(
    catalog: &ComponentVariantCatalog,
    preferred_height: PreferredHeightPolicy,
) -> Result<ComponentVariantSelection, DashRepresentationLaneCatalogBuildError> {
    let mut ranked_video = catalog
        .required_video_variants()
        .map_or_else(|_| Vec::new(), |video| video.iter().collect::<Vec<_>>());
    ranked_video.sort_by(|left, right| {
        preferred_height.compare(left.track().height(), right.track().height())
    });

    if let (Some(compatibility), Ok(audio)) =
        (catalog.compatibility(), catalog.required_audio_variants())
    {
        for video in &ranked_video {
            if let Some(audio) = best_default_audio(audio.iter(), |audio| {
                compatibility.allows(video.exact_identity(), audio.exact_identity())
            }) {
                return catalog
                    .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                        video: video.exact_identity().clone(),
                        audio: audio.exact_identity().clone(),
                    })
                    .map_err(Into::into);
            }
        }
    }

    let coupled = catalog
        .coupled_presentations()
        .iter()
        .min_by(|left, right| {
            // Больший tier лучше, поэтому сравнение tier-ов развёрнуто (right vs left).
            audio_tier(right.audio())
                .cmp(&audio_tier(left.audio()))
                .then_with(|| {
                    preferred_height.compare(left.video().height(), right.video().height())
                })
        });
    if let Some(coupled) = coupled {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::Coupled {
                presentation: coupled.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    if let Some(video) = ranked_video
        .into_iter()
        .find(|video| catalog.is_video_only_selectable(video.exact_identity()))
    {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::VideoOnly {
                video: video.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    if let Ok(audio) = catalog.required_audio_variants()
        && let Some(audio) = best_default_audio(audio.iter(), |audio| {
            catalog.is_audio_only_selectable(audio.exact_identity())
        })
    {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    Err(DashRepresentationLaneCatalogBuildError::NoSelectableLane)
}

/// Default-уровень audio codec-а описанной дорожки.
fn audio_tier(track: &AudioTrackDescriptor) -> DefaultAudioSelectionTier {
    DefaultAudioSelectionTier::for_codec(track.codec().kind())
}

/// Первый (в порядке catalog-а) подходящий audio высшего доступного tier-а.
///
/// Сохраняет прежний порядок «первый подходящий» внутри tier-а; `FallbackOnly`
/// рассматривается только при отсутствии подходящего `Primary`.
fn best_default_audio<'catalog>(
    candidates: impl Iterator<Item = &'catalog AudioComponentVariant>,
    mut is_eligible: impl FnMut(&AudioComponentVariant) -> bool,
) -> Option<&'catalog AudioComponentVariant> {
    let mut fallback_only = None;
    for audio in candidates.filter(|audio| is_eligible(audio)) {
        match audio_tier(audio.track()) {
            DefaultAudioSelectionTier::Primary => return Some(audio),
            DefaultAudioSelectionTier::FallbackOnly => {
                fallback_only = fallback_only.or(Some(audio));
            }
        }
    }
    fallback_only
}

fn unique_default_row<'rows>(
    rows: &'rows [PublishedLane],
    presentation: &DashMpd,
    evidence: &DashRepresentationEvidence,
) -> Result<&'rows PublishedLane, DashRepresentationLaneCatalogBuildError> {
    let mut matches = rows
        .iter()
        .filter(|row| lane_matches_evidence(&row.lane, presentation, evidence));
    let first = matches
        .next()
        .ok_or(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing)?;
    if matches.next().is_some() {
        return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultAmbiguous);
    }
    Ok(first)
}

#[cfg(test)]
#[path = "default_selection_tests.rs"]
mod tests;
