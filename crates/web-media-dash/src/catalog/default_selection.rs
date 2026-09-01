//! Provider-default выбирается только из доказанных catalog rows и compatibility edges.

use super::*;

pub(super) fn provider_default_selection(
    catalog: &ComponentVariantCatalog,
    rows: &[PublishedLane],
    presentation: &DashMpd,
    provider_default: &DashPresentationSelection,
) -> Result<ComponentVariantSelection, DashRepresentationLaneCatalogBuildError> {
    match provider_default {
        DashPresentationSelection::Single { main } => {
            let row = unique_default_row(rows, presentation, main)?;
            let request = match row.kind {
                DashMediaKind::Video => ComponentVariantSelectionRequest::VideoOnly {
                    video: row
                        .component_exact
                        .clone()
                        .expect("video default invariant"),
                },
                DashMediaKind::Audio => ComponentVariantSelectionRequest::AudioOnly {
                    audio: row
                        .component_exact
                        .clone()
                        .expect("audio default invariant"),
                },
                DashMediaKind::Muxed => ComponentVariantSelectionRequest::Coupled {
                    presentation: row.coupled_exact.clone().expect("muxed default invariant"),
                },
            };
            catalog.select_exact(request).map_err(Into::into)
        }
        DashPresentationSelection::Separate { video, audio } => {
            let video = unique_default_row(rows, presentation, video)?;
            let audio = unique_default_row(rows, presentation, audio)?;
            if video.kind != DashMediaKind::Video || audio.kind != DashMediaKind::Audio {
                return Err(DashRepresentationLaneCatalogBuildError::ProviderDefaultMissing);
            }
            catalog
                .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
                    video: video
                        .component_exact
                        .clone()
                        .expect("video default invariant"),
                    audio: audio
                        .component_exact
                        .clone()
                        .expect("audio default invariant"),
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
            if let Some(audio) = audio
                .iter()
                .find(|audio| compatibility.allows(video.exact_identity(), audio.exact_identity()))
            {
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
            preferred_height.compare(left.video().height(), right.video().height())
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
        && let Some(audio) = audio
            .iter()
            .find(|audio| catalog.is_audio_only_selectable(audio.exact_identity()))
    {
        return catalog
            .select_exact(ComponentVariantSelectionRequest::AudioOnly {
                audio: audio.exact_identity().clone(),
            })
            .map_err(Into::into);
    }

    Err(DashRepresentationLaneCatalogBuildError::NoSelectableLane)
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
