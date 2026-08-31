//! Generic normalization public serialized yt-dlp format descriptors.
//!
//! Модуль является service-owned anti-corruption boundary: raw JSON и
//! transient request material остаются здесь, а наружу выдаются нейтральные
//! [`web_media_core`] descriptors и secret-safe service values.

mod composition;
mod descriptor;
mod model;
mod normalize;
mod planning;
mod raw;
mod rematch;
mod request_material;
mod transport;

pub use composition::{YtDlpComposedSelection, YtDlpCompositionError, YtDlpCompositionMatchKind};
pub use model::{
    YtDlpCandidateComponentRequestSummary, YtDlpCandidateComponentRole, YtDlpCandidateEntry,
    YtDlpCandidateMatch, YtDlpCandidateMatchKind, YtDlpCandidateNormalizationRejection,
    YtDlpCandidateOrigin, YtDlpCandidateRematchError, YtDlpCandidateSelection,
    YtDlpCandidateSelectionError, YtDlpCandidateSnapshot, YtDlpLiveIntent,
    YtDlpNormalizedCandidate, YtDlpRejectedCandidate, YtDlpSelectedCandidateShape,
};
pub use planning::{
    YtDlpPlanningCandidateRejection, YtDlpPlanningCandidateRejectionReason,
    YtDlpPlanningProjection, YtDlpPlanningSnapshotAlignmentError, YtDlpPlanningSnapshotError,
};
pub use request_material::{
    YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION, YtDlpDashFragment, YtDlpDashFragmentLocatorKind,
    YtDlpDashFragmentRole, YtDlpDashInput, YtDlpDashInputKind, YtDlpDashRequestContext,
    YtDlpDashRequestMaterial, YtDlpDashRequestMaterialViolation, YtDlpHdsManifestRequestMaterial,
    YtDlpHdsManifestRequestMaterialViolation, YtDlpHlsAesOverride, YtDlpHlsManifestInput,
    YtDlpHlsManifestInputKind, YtDlpHlsRequestMaterial, YtDlpHlsRequestMaterialViolation,
    YtDlpRequestMaterial, YtDlpRequestMaterialSummary, YtDlpRequestMaterialV1,
    YtDlpRequestMaterialViolation, YtDlpSmoothManifestRequestMaterial,
    YtDlpSmoothManifestRequestMaterialViolation, YtDlpSmoothUnsupportedRequestMaterial,
};
pub use transport::{
    YtDlpDashTransportComponent, YtDlpProgressiveTransportRequestContext, YtDlpTransportComponent,
    YtDlpTransportRequestContext, YtDlpTransportRequestError,
};

pub(crate) use normalize::normalize_candidate_document;
#[cfg(test)]
pub(crate) use raw::YtDlpCandidateDocument;

use rustiplayer_config::YtDlpConfig;
use web_media_core::{ExtractionGeneration, ExtractorInvocationReason, SourceIdentity};

use crate::error::YtDlpServiceError;
use crate::invocation::YtDlpExtractorAdapter;
use crate::locator::YtDlpMediaLocator;
use crate::process::{YtDlpProcessConfig, resolve_yt_dlp_candidate_document_with_cancellation};

/// Извлекает public format inventory и нормализует immutable S19 snapshot.
pub fn resolve_yt_dlp_candidate_snapshot_with_config(
    locator: &YtDlpMediaLocator,
    source: SourceIdentity,
    generation: ExtractionGeneration,
    yt_dlp_config: &YtDlpConfig,
) -> Result<YtDlpCandidateSnapshot, YtDlpServiceError> {
    YtDlpExtractorAdapter::default().resolve_candidate_snapshot_with_cancellation(
        locator,
        source,
        generation,
        yt_dlp_config,
        ExtractorInvocationReason::PageMediaResolution,
        &|| false,
    )
}

/// Извлекает S19 snapshot и останавливает owned `yt-dlp` process при отмене caller-а.
pub fn resolve_yt_dlp_candidate_snapshot_with_config_and_cancellation(
    locator: &YtDlpMediaLocator,
    source: SourceIdentity,
    generation: ExtractionGeneration,
    yt_dlp_config: &YtDlpConfig,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<YtDlpCandidateSnapshot, YtDlpServiceError> {
    YtDlpExtractorAdapter::default().resolve_candidate_snapshot_with_cancellation(
        locator,
        source,
        generation,
        yt_dlp_config,
        ExtractorInvocationReason::PageMediaResolution,
        is_cancelled,
    )
}

/// Реализует adapter method с explicit reason и injected launcher-ом.
pub(crate) fn resolve_candidate_snapshot_with_adapter(
    adapter: &YtDlpExtractorAdapter,
    locator: &YtDlpMediaLocator,
    source: SourceIdentity,
    generation: ExtractionGeneration,
    yt_dlp_config: &YtDlpConfig,
    invocation_reason: ExtractorInvocationReason,
    is_cancelled: &dyn Fn() -> bool,
) -> Result<YtDlpCandidateSnapshot, YtDlpServiceError> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpServiceError::AdapterDisabled);
    }
    let process_config = YtDlpProcessConfig::from_yt_dlp_config_with_invocation(
        yt_dlp_config,
        adapter.process_launcher(),
        invocation_reason,
    )?;
    let document = resolve_yt_dlp_candidate_document_with_cancellation(
        locator,
        &process_config,
        is_cancelled,
    )?;
    Ok(normalize_candidate_document(document, source, generation))
}

#[cfg(test)]
mod planning_tests;

#[cfg(test)]
mod rematch_tests;

#[cfg(test)]
mod tests;
