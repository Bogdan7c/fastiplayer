use super::*;

/// Extraction generation первого immutable candidate snapshot-а.
const INITIAL_EXTRACTION_GENERATION: u64 = 1;

pub(super) enum ResolvedCandidateIntent {
    Planner(SelectionRequest),
    Composed {
        candidate: Box<YtDlpNormalizedCandidate>,
        selection: Box<service_ytdlp::YtDlpComposedSelection>,
        parent_preference: Box<YtDlpCandidateSelection>,
    },
}

/// Выдаёт non-zero generation отдельному dynamic timeline port-у.
pub(crate) fn next_dynamic_timeline_port_generation() -> Result<DynamicMediaTimelinePortGeneration>
{
    let generation_value = NEXT_DYNAMIC_TIMELINE_PORT_GENERATION
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("dynamic timeline port generation space исчерпан"))?;
    let generation_value = NonZeroU64::new(generation_value)
        .ok_or_else(|| anyhow!("dynamic timeline port generation не может быть нулевым"))?;
    Ok(DynamicMediaTimelinePortGeneration::new(generation_value))
}

/// Выдаёт отдельную app-owned generation свежему component catalog-у.
pub(super) fn next_component_variant_catalog_generation()
-> Result<web_media_core::ComponentVariantCatalogGeneration> {
    allocate_component_variant_catalog_generation(&NEXT_COMPONENT_VARIANT_CATALOG_GENERATION)
}

/// Выдаёт generation из переданного authority-owned allocator-а.
pub(super) fn allocate_component_variant_catalog_generation(
    allocator: &AtomicU64,
) -> Result<web_media_core::ComponentVariantCatalogGeneration> {
    let generation_value = allocator
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |current| {
            current.checked_add(1)
        })
        .map_err(|_| anyhow!("component variant catalog generation space исчерпан"))?;
    if generation_value == 0 {
        bail!("component variant catalog generation не может быть нулевой");
    }
    Ok(web_media_core::ComponentVariantCatalogGeneration::new(
        generation_value,
    ))
}

/// Разрешает fresh snapshot либо re-extract + semantic rematch для exact restore.
pub(super) fn resolve_candidate_snapshot(
    locator: &YtDlpMediaLocator,
    yt_dlp_config: &YtDlpConfig,
    intent: YtDlpCandidateOpenIntent,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<(YtDlpCandidateSnapshot, ResolvedCandidateIntent)> {
    match intent {
        YtDlpCandidateOpenIntent::BestPlayable => {
            let source = next_source_identity()?;
            let generation = ExtractionGeneration::new(INITIAL_EXTRACTION_GENERATION);
            let snapshot = service_ytdlp::YtDlpExtractorAdapter::default()
                .resolve_candidate_snapshot_with_cancellation(
                    locator,
                    source,
                    generation,
                    yt_dlp_config,
                    web_media_core::ExtractorInvocationReason::PageMediaResolution,
                    is_cancelled,
                )?;
            Ok((
                snapshot,
                ResolvedCandidateIntent::Planner(SelectionRequest::BestPlayable),
            ))
        }
        YtDlpCandidateOpenIntent::Exact(exact) => {
            let previous = exact.selection;
            let source = previous.exact_identity().source();
            let generation_value = previous
                .exact_identity()
                .generation()
                .value()
                .checked_add(1)
                .ok_or_else(|| anyhow!("YtDlp extraction generation space исчерпан"))?;
            let snapshot = service_ytdlp::YtDlpExtractorAdapter::default()
                .resolve_candidate_snapshot_with_cancellation(
                    locator,
                    source,
                    ExtractionGeneration::new(generation_value),
                    yt_dlp_config,
                    web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                    is_cancelled,
                )?;
            let matched = snapshot
                .rematch_exact(&previous)
                .context("Fresh YtDlp snapshot не содержит semantic match exact selection")?;
            let exact = ExactSelectionIdentity::new(
                matched.candidate().descriptor().identity().clone(),
                matched.candidate().descriptor().semantic_identity().clone(),
            )
            .context("Rematched YtDlp identities нарушают source lineage")?;
            Ok((
                snapshot,
                ResolvedCandidateIntent::Planner(SelectionRequest::Exact(exact)),
            ))
        }
        YtDlpCandidateOpenIntent::Composed(composed) => {
            if composed.parent_preference.as_ref() != composed.selection.video_parent_selection() {
                return Err(anyhow!(
                    "Composed YtDlp intent содержит несогласованный video parent selection"
                ));
            }
            let source = composed.selection.descriptor().identity().source();
            let generation_value = composed
                .selection
                .descriptor()
                .identity()
                .generation()
                .value()
                .checked_add(1)
                .ok_or_else(|| anyhow!("YtDlp extraction generation space исчерпан"))?;
            let snapshot = service_ytdlp::YtDlpExtractorAdapter::default()
                .resolve_candidate_snapshot_with_cancellation(
                    locator,
                    source,
                    ExtractionGeneration::new(generation_value),
                    yt_dlp_config,
                    web_media_core::ExtractorInvocationReason::ExtractorBackedRecovery,
                    is_cancelled,
                )?;
            let (_, selection, candidate) = snapshot
                .rematch_composed(&composed.selection)
                .context("Fresh YtDlp snapshot не содержит composed semantic match")?;
            let parent_preference = Box::new(selection.video_parent_selection().clone());
            Ok((
                snapshot,
                ResolvedCandidateIntent::Composed {
                    candidate: Box::new(candidate),
                    selection: Box::new(selection),
                    parent_preference,
                },
            ))
        }
    }
}
