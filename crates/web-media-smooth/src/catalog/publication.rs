//! Atomic neutral catalog publication after provider-owned row preparation.

use super::*;

/// Единственная atomic publication path для default и discovered rows.
pub(crate) fn publish_catalog(
    request: SmoothCatalogBuildRequest<'_>,
    mut video_rows: Vec<PendingVideoRow>,
    mut audio_rows: Vec<PendingAudioRow>,
) -> Result<SmoothCatalogBuild, SmoothPrepareError> {
    video_rows.sort_by(|left, right| {
        right
            .height
            .cmp(&left.height)
            .then_with(|| right.width.cmp(&left.width))
            .then_with(|| right.bitrate.cmp(&left.bitrate))
            .then_with(|| left.canonical_key.cmp(&right.canonical_key))
    });
    audio_rows.sort_by(|left, right| {
        right
            .bitrate
            .cmp(&left.bitrate)
            .then_with(|| right.sample_rate.cmp(&left.sample_rate))
            .then_with(|| right.channels.cmp(&left.channels))
            .then_with(|| left.canonical_key.cmp(&right.canonical_key))
    });
    require_not_cancelled(request.cancellation)?;

    let video_variants = video_rows
        .iter()
        .map(|row| row.variant.clone())
        .collect::<Vec<_>>();
    let audio_variants = audio_rows
        .iter()
        .map(|row| row.variant.clone())
        .collect::<Vec<_>>();
    require_not_cancelled(request.cancellation)?;
    let catalog = ComponentVariantCatalog::new(
        request.catalog_identity,
        request.policy.catalog_limit,
        ComponentVariantCatalogEntries::Topology {
            video: video_variants,
            audio: audio_variants,
            compatibility: ComponentVariantCompatibilityEntries::AllPairs {
                edge_limit: request.policy.compatibility_edge_limit,
            },
            coupled: Vec::new(),
            video_only: Vec::new(),
            audio_only: Vec::new(),
        },
    )
    .map_err(SmoothPrepareError::Catalog)?;

    let preferred_video = catalog
        .preferred_video_variant(request.preferred_height)
        .map_err(SmoothPrepareError::Catalog)?
        .exact_identity()
        .clone();
    let preferred_audio = catalog
        .required_audio_variants()
        .map_err(SmoothPrepareError::Catalog)?
        .first()
        .ok_or(SmoothProfileError::EmptyQualityAxis)?
        .exact_identity()
        .clone();
    let provider_selection = catalog
        .select_exact(ComponentVariantSelectionRequest::VideoAndAudio {
            video: preferred_video,
            audio: preferred_audio,
        })
        .map_err(SmoothPrepareError::Catalog)?;

    let prepared = SmoothCatalogBuild {
        catalog,
        provider_selection,
        video_rows: video_rows
            .into_iter()
            .map(|row| row.runtime)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
        audio_rows: audio_rows
            .into_iter()
            .map(|row| row.runtime)
            .collect::<Vec<_>>()
            .into_boxed_slice(),
    };
    require_not_cancelled(request.cancellation)?;
    Ok(prepared)
}
