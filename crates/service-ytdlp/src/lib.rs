//! Service boundary system `yt-dlp` extractor-а.
//!
//! Crate владеет locator-ами, process invocation, topology/metadata и S19
//! candidate/request-material normalization. Playback transport и demux здесь
//! намеренно отсутствуют: composition root выбирает candidate через S21C и
//! открывает его зарегистрированным S22 provider-ом.

mod candidate;
mod dto;
mod embed_recovery;
mod error;
mod invocation;
mod locator;
mod metadata;
mod process;
mod process_output;
mod process_tree;
mod topology;

pub use candidate::{
    YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION, YtDlpCandidateComponentRequestSummary,
    YtDlpCandidateComponentRole, YtDlpCandidateEntry, YtDlpCandidateMatch, YtDlpCandidateMatchKind,
    YtDlpCandidateNormalizationRejection, YtDlpCandidateOrigin, YtDlpCandidateRematchError,
    YtDlpCandidateSelection, YtDlpCandidateSelectionError, YtDlpCandidateSnapshot,
    YtDlpComposedSelection, YtDlpCompositionError, YtDlpCompositionMatchKind, YtDlpDashFragment,
    YtDlpDashFragmentLocatorKind, YtDlpDashFragmentRole, YtDlpDashInput, YtDlpDashInputKind,
    YtDlpDashRequestContext, YtDlpDashRequestMaterial, YtDlpDashRequestMaterialViolation,
    YtDlpDashTransportComponent, YtDlpHdsManifestRequestMaterial,
    YtDlpHdsManifestRequestMaterialViolation, YtDlpHlsAesOverride, YtDlpHlsManifestInput,
    YtDlpHlsManifestInputKind, YtDlpHlsRequestMaterial, YtDlpHlsRequestMaterialViolation,
    YtDlpLiveIntent, YtDlpNormalizedCandidate, YtDlpPlanningCandidateRejection,
    YtDlpPlanningCandidateRejectionReason, YtDlpPlanningProjection,
    YtDlpPlanningSnapshotAlignmentError, YtDlpPlanningSnapshotError,
    YtDlpProgressiveTransportRequestContext, YtDlpRejectedCandidate, YtDlpRequestMaterial,
    YtDlpRequestMaterialSummary, YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
    YtDlpSelectedCandidateShape, YtDlpSmoothManifestRequestMaterial,
    YtDlpSmoothManifestRequestMaterialViolation, YtDlpSmoothUnsupportedRequestMaterial,
    YtDlpTransportComponent, YtDlpTransportRequestContext, YtDlpTransportRequestError,
    resolve_yt_dlp_candidate_snapshot_with_config,
    resolve_yt_dlp_candidate_snapshot_with_config_and_cancellation,
};
pub use error::YtDlpServiceError;
pub use invocation::{
    ExtractorProcessInvocation, ExtractorProcessLauncher, ExtractorProcessPhase,
    YtDlpExtractorAdapter,
};
pub use locator::{
    YtDlpInputScheme, YtDlpLocatorParseError, YtDlpMediaLocator, parse_yt_dlp_media_locator,
};
pub use metadata::{YtDlpPlaylistMetadata, resolve_yt_dlp_playlist_metadata_with_config};
pub use topology::{
    DEFAULT_TOPOLOGY_DEPTH, DEFAULT_TOPOLOGY_ENTRY_COUNT, DEFAULT_TOPOLOGY_JSON_DEPTH,
    DEFAULT_TOPOLOGY_JSON_LINE_BYTES, DEFAULT_TOPOLOGY_STDERR_BYTES, DEFAULT_TOPOLOGY_STDOUT_BYTES,
    TOPOLOGY_IDENTITY_MAX_UTF8_BYTES, TOPOLOGY_LOCATOR_MAX_UTF8_BYTES,
    TOPOLOGY_SUMMARY_TEXT_MAX_UTF8_BYTES, YT_DLP_DURABLE_REOPEN_PAYLOAD_MAX_BYTES,
    YT_DLP_DURABLE_REOPEN_PAYLOAD_VERSION, YT_DLP_DURABLE_REOPEN_SERVICE_OWNER,
    YtDlpDelegationSummaryPolicy, YtDlpDurableReopenClassificationError,
    YtDlpDurableReopenIdentityInput, YtDlpDurableReopenMaterialKind, YtDlpDurableReopenPayload,
    YtDlpTopology, YtDlpTopologyBudgetField, YtDlpTopologyBudgets, YtDlpTopologyCollection,
    YtDlpTopologyDelegation, YtDlpTopologyEntry, YtDlpTopologyEntryKind, YtDlpTopologyError,
    YtDlpTopologyIdentity, YtDlpTopologyInvalidResponseReason, YtDlpTopologyKind,
    YtDlpTopologyMultiVideo, YtDlpTopologySummary, YtDlpTopologySummaryFieldState,
    YtDlpTopologySummaryUnavailableReason, YtDlpTopologyVideo, YtDlpUnavailableTopologyEntry,
    YtDlpUnavailableTopologyReason, classify_yt_dlp_delegation_reopen_target,
    classify_yt_dlp_durable_reopen_identity, extract_yt_dlp_topology_with_budgets,
    extract_yt_dlp_topology_with_config,
};

/// Отличает authority-style network URL от обычного local path.
#[must_use]
pub fn is_probably_url(argument: &str) -> bool {
    argument.contains("://")
}

/// Проверяет, входит ли absolute URL в S00-approved `yt-dlp` input vocabulary.
#[must_use]
pub fn is_supported_yt_dlp_url(argument: &str) -> bool {
    parse_yt_dlp_media_locator(argument).is_ok()
}
