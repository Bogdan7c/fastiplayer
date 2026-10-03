//! Bounded retry orchestration для planner-ranked ContentProbe failures.
//!
//! Retry разрешён только BestPlayable intent-у после typed runtime content
//! rejection либо одного typed `NetworkUnavailable` candidate-а.
//! Timeout, authentication, cancellation, parser и остальные provider failures
//! завершают open немедленно и не умножают wall-clock latency.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use fastiplayer_config::{NetworkConfig, YtDlpConfig};
use service_ytdlp::{
    YtDlpCandidateSelection, YtDlpCandidateSnapshot, YtDlpMediaLocator, YtDlpNormalizedCandidate,
};
use source_core::CancellationToken;
use web_media_core::{CandidateIdentity, SemanticIdentity};
use web_media_dash::DashEndpointRefreshPort;
use web_media_hls::HlsEndpointRefreshPort;
use web_media_playback_plan::{
    PlanningCandidateSnapshot, PlaybackCapabilitySnapshot, PlaybackSelectionPolicy,
    rank_playable_opaque_alternatives,
};
use web_media_transport_api::{TransportFailure, TransportOpenError};

use super::content_probe::ContentProbeRejection;
use super::{
    AdaptiveEndpointRefreshPorts, OpenedWebCandidate, WebCandidateOpenContext, WebOpenRuntime,
    catalog_capabilities::AppCatalogCapabilityProbe,
    component_variants::YtDlpComponentSelectionOpenIntent, preparation,
};

/// BestPlayable может обойти ровно один недоступный physical candidate.
///
/// Это не transport retry: следующая попытка использует другую planner identity.
/// Лимит не позволяет большим inventories последовательно умножать network latency.
const MAX_NETWORK_UNAVAILABLE_FALLBACKS: usize = 1;

/// Named зависимости открытия одного concrete candidate-а.
///
/// Контекст живёт только внутри одного immutable extraction snapshot-а. Retry
/// меняет candidate/selection, но не policy, cancellation generation или
/// provider registries.
pub(super) struct CandidateAttemptContext<'context, IsCancelled>
where
    IsCancelled: Fn() -> bool,
{
    pub(super) locator: &'context YtDlpMediaLocator,
    pub(super) network_config: &'context NetworkConfig,
    pub(super) yt_dlp_config: &'context YtDlpConfig,
    pub(super) extractor_adapter: &'context service_ytdlp::YtDlpExtractorAdapter,
    pub(super) candidate_snapshot: &'context YtDlpCandidateSnapshot,
    pub(super) runtime: &'context WebOpenRuntime,
    pub(super) component_selection_intent: &'context YtDlpComponentSelectionOpenIntent,
    pub(super) preferred_height: web_media_core::PreferredHeightPolicy,
    pub(super) cancellation: &'context CancellationToken,
    pub(super) is_cancelled: &'context IsCancelled,
    pub(super) playback_policy: &'context PlaybackSelectionPolicy,
    pub(super) catalog_capability_probe: &'context mut AppCatalogCapabilityProbe,
}

/// Успешная concrete попытка вместе с exact active selection.
pub(super) struct OpenedCandidateAttempt {
    candidate_selection: YtDlpCandidateSelection,
    opened_candidate: OpenedWebCandidate,
}

impl OpenedCandidateAttempt {
    /// Передаёт ownership orchestration layer-у после выбора успешной попытки.
    pub(super) fn into_parts(self) -> (YtDlpCandidateSelection, OpenedWebCandidate) {
        (self.candidate_selection, self.opened_candidate)
    }
}

impl<IsCancelled> CandidateAttemptContext<'_, IsCancelled>
where
    IsCancelled: Fn() -> bool,
{
    /// Собирает candidate-specific refresh/catalog state и открывает одну попытку.
    pub(super) fn open(
        &mut self,
        candidate: &YtDlpNormalizedCandidate,
        candidate_selection: YtDlpCandidateSelection,
    ) -> std::result::Result<OpenedCandidateAttempt, CandidateOpenError> {
        let catalog_identity = web_media_core::ComponentVariantCatalogIdentity::new(
            web_media_core::ExactSelectionIdentity::new(
                candidate_selection.exact_identity().clone(),
                candidate_selection.semantic_identity().clone(),
            )
            .context("YtDlp catalog parent identities нарушают source lineage")?,
            preparation::next_component_variant_catalog_generation()?,
        );
        let hls_endpoint_refresh: Option<Arc<dyn HlsEndpointRefreshPort>> =
            (self.candidate_snapshot.live_intent() == service_ytdlp::YtDlpLiveIntent::Live
                && crate::web_media_hls_open::candidate_is_hls(candidate))
            .then(|| {
                Arc::new(
                    crate::web_media_hls_refresh::AppHlsEndpointRefreshPort::new(
                        self.locator.clone(),
                        self.yt_dlp_config.clone(),
                        self.extractor_adapter.clone(),
                        self.network_config.clone(),
                        self.runtime.source_config.clone(),
                        self.runtime.provider_id.clone(),
                        candidate_selection.clone(),
                        self.cancellation.clone(),
                    ),
                ) as Arc<dyn HlsEndpointRefreshPort>
            });
        let dash_endpoint_refresh: Option<Arc<dyn DashEndpointRefreshPort>> =
            (self.candidate_snapshot.live_intent() == service_ytdlp::YtDlpLiveIntent::Live
                && crate::web_media_dash_open::candidate_is_dash(candidate))
            .then(|| {
                Arc::new(
                    crate::web_media_dash_refresh::AppDashEndpointRefreshPort::new(
                        self.locator.clone(),
                        self.yt_dlp_config.clone(),
                        self.extractor_adapter.clone(),
                        self.network_config.clone(),
                        self.runtime.source_config.clone(),
                        self.runtime.provider_id.clone(),
                        candidate_selection.clone(),
                        self.cancellation.clone(),
                    ),
                ) as Arc<dyn DashEndpointRefreshPort>
            });
        let vod_endpoint_recovery = (self.candidate_snapshot.live_intent()
            != service_ytdlp::YtDlpLiveIntent::Live)
            .then(crate::web_media_vod_recovery::VodEndpointRecoveryAttachment::new);
        let opened_candidate = self.runtime.open_candidate(
            candidate,
            WebCandidateOpenContext {
                live_intent: self.candidate_snapshot.live_intent(),
                endpoint_refresh_ports: AdaptiveEndpointRefreshPorts {
                    hls: hls_endpoint_refresh,
                    dash: dash_endpoint_refresh,
                },
                timeline_port_generation: preparation::next_dynamic_timeline_port_generation()?,
                component_selection_intent: self.component_selection_intent.clone(),
                preferred_height: self.preferred_height,
                catalog_identity,
                cancellation: self.cancellation.clone(),
                vod_endpoint_recovery,
            },
            self.is_cancelled,
            self.catalog_capability_probe,
            self.playback_policy,
        )?;
        Ok(OpenedCandidateAttempt {
            candidate_selection,
            opened_candidate,
        })
    }
}

/// Typed граница между retryable content proof и terminal open failure.
#[derive(Debug)]
pub(super) enum CandidateOpenError {
    /// Candidate physical resource открылся, но actual tracks не прошли proof.
    ContentProbe(ContentProbeRejection),
    /// Concrete candidate не открылся без timeout-а; BestPlayable может попробовать один alternate.
    NetworkUnavailable(anyhow::Error),
    /// Любая ошибка вне content-proof contract-а остаётся terminal.
    Fatal(anyhow::Error),
}

impl CandidateOpenError {
    /// Возвращает исходную typed ошибку для single-attempt Exact/Composed path-а.
    pub(super) fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::ContentProbe(rejection) => anyhow::Error::new(rejection),
            Self::NetworkUnavailable(error) | Self::Fatal(error) => error,
        }
    }
}

impl From<anyhow::Error> for CandidateOpenError {
    fn from(error: anyhow::Error) -> Self {
        if matches!(
            error.downcast_ref::<TransportOpenError>(),
            Some(TransportOpenError::Transport(
                TransportFailure::NetworkUnavailable
            ))
        ) {
            Self::NetworkUnavailable(error)
        } else {
            Self::Fatal(error)
        }
    }
}

impl From<ContentProbeRejection> for CandidateOpenError {
    fn from(rejection: ContentProbeRejection) -> Self {
        Self::ContentProbe(rejection)
    }
}

/// Проецирует accepted service candidates в exact planner-owned best-first rank.
pub(super) fn ranked_best_playable_candidates<'candidate>(
    candidate_snapshot: &'candidate YtDlpCandidateSnapshot,
    planning_snapshot: &PlanningCandidateSnapshot,
    capabilities: PlaybackCapabilitySnapshot<'_>,
    policy: &PlaybackSelectionPolicy,
) -> Result<Vec<&'candidate YtDlpNormalizedCandidate>> {
    candidate_snapshot
        .validate_planning_snapshot_alignment(planning_snapshot)
        .context("Service/planner candidate snapshots не соответствуют друг другу")?;
    let ranking = rank_playable_opaque_alternatives(planning_snapshot, capabilities, policy)
        .context("Не удалось получить planner-owned BestPlayable ranking")?;
    map_ranked_service_candidates(candidate_snapshot, ranking.ranked_candidate_identities())
}

/// Сопоставляет полный planner rank с canonical service candidates по identity.
fn map_ranked_service_candidates<'candidate, 'ranked_identity>(
    candidate_snapshot: &'candidate YtDlpCandidateSnapshot,
    ranked_identities: impl ExactSizeIterator<
        Item = (
            &'ranked_identity CandidateIdentity,
            &'ranked_identity SemanticIdentity,
        ),
    >,
) -> Result<Vec<&'candidate YtDlpNormalizedCandidate>> {
    let mut canonical_candidates = BTreeMap::new();
    for candidate in candidate_snapshot.accepted_candidates() {
        let identity = (
            candidate.descriptor().identity().clone(),
            candidate.descriptor().semantic_identity().clone(),
        );
        if canonical_candidates.insert(identity, candidate).is_some() {
            bail!("Service snapshot содержит duplicate canonical candidate identity");
        }
    }

    let mut ranked_candidates = Vec::with_capacity(ranked_identities.len());
    for (exact_identity, semantic_identity) in ranked_identities {
        let identity = (exact_identity.clone(), semantic_identity.clone());
        let candidate = canonical_candidates.remove(&identity).ok_or_else(|| {
            anyhow::anyhow!("Planner ranking нельзя полностью сопоставить service snapshot-у")
        })?;
        ranked_candidates.push(candidate);
    }
    if ranked_candidates.is_empty() {
        bail!("Planner ranking не содержит BestPlayable candidate");
    }
    Ok(ranked_candidates)
}

/// Открывает BestPlayable candidates по bounded planner rank до первого успеха.
pub(super) fn open_ranked_best<'candidate, Candidate, Opened>(
    candidates: impl IntoIterator<Item = &'candidate Candidate>,
    is_cancelled: &impl Fn() -> bool,
    mut open: impl FnMut(&'candidate Candidate) -> std::result::Result<Opened, CandidateOpenError>,
) -> Result<(&'candidate Candidate, Opened)> {
    let mut rejection_count = 0_usize;
    let mut network_unavailable_count = 0_usize;
    let mut last_retryable_error = None;
    for candidate in candidates {
        if is_cancelled() {
            bail!("YtDlp candidate fallback отменён до следующей попытки");
        }
        match open(candidate) {
            Ok(opened) => return Ok((candidate, opened)),
            Err(CandidateOpenError::ContentProbe(rejection)) => {
                rejection_count = rejection_count.saturating_add(1);
                last_retryable_error = Some(CandidateOpenError::ContentProbe(rejection));
            }
            Err(CandidateOpenError::NetworkUnavailable(error)) => {
                network_unavailable_count = network_unavailable_count.saturating_add(1);
                if network_unavailable_count > MAX_NETWORK_UNAVAILABLE_FALLBACKS {
                    return Err(error.context(format!(
                        "BestPlayable network fallback исчерпан (content_rejections={rejection_count}, unavailable_candidates={network_unavailable_count})"
                    )));
                }
                last_retryable_error = Some(CandidateOpenError::NetworkUnavailable(error));
            }
            Err(CandidateOpenError::Fatal(error)) => return Err(error),
        }
    }

    let exhausted_context = format!(
        "Planner-ranked BestPlayable candidates исчерпаны (content_rejections={rejection_count}, unavailable_candidates={network_unavailable_count})"
    );
    match last_retryable_error {
        Some(CandidateOpenError::ContentProbe(rejection)) => {
            Err(anyhow::Error::new(rejection).context(exhausted_context))
        }
        Some(CandidateOpenError::NetworkUnavailable(error)) => {
            Err(error.context(exhausted_context))
        }
        // Fatal не сохраняется для fallback-а, но match остаётся total при
        // дальнейшем расширении CandidateOpenError.
        Some(CandidateOpenError::Fatal(error)) => Err(error),
        None => bail!("Planner ranking не предоставил candidate для открытия"),
    }
}

/// Выполняет ровно одну Exact/Composed попытку без скрытого fallback-а.
pub(super) fn open_single<Candidate, Opened>(
    candidate: &Candidate,
    open: impl FnOnce(&Candidate) -> std::result::Result<Opened, CandidateOpenError>,
) -> Result<Opened> {
    open(candidate).map_err(CandidateOpenError::into_anyhow)
}

#[cfg(test)]
#[path = "content_probe_fallback/tests.rs"]
mod tests;
