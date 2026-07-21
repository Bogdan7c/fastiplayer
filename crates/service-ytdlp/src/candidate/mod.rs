//! Generic normalization public serialized yt-dlp format descriptors.
//!
//! Модуль является service-owned anti-corruption boundary: raw JSON и
//! transient request material остаются здесь, а наружу выдаются нейтральные
//! [`web_media_core`] descriptors и secret-safe service values.

mod descriptor;
mod model;
mod normalize;
mod raw;
mod request_material;

pub use model::{
    YtDlpCandidateComponentRequestSummary, YtDlpCandidateComponentRole, YtDlpCandidateEntry,
    YtDlpCandidateMatch, YtDlpCandidateMatchKind, YtDlpCandidateNormalizationRejection,
    YtDlpCandidateOrigin, YtDlpCandidateRematchError, YtDlpCandidateSelection,
    YtDlpCandidateSelectionError, YtDlpCandidateSnapshot, YtDlpNormalizedCandidate,
    YtDlpRejectedCandidate, YtDlpSelectedCandidateShape,
};
pub use request_material::{
    YT_DLP_REQUEST_MATERIAL_SCHEMA_VERSION, YtDlpRequestMaterial, YtDlpRequestMaterialSummary,
    YtDlpRequestMaterialV1, YtDlpRequestMaterialViolation,
};

pub(crate) use normalize::normalize_candidate_document;
pub(crate) use raw::YtDlpCandidateDocument;

use rustiplayer_config::YtDlpConfig;
use web_media_core::{ExtractionGeneration, SourceIdentity};

use crate::error::YtDlpServiceError;
use crate::locator::YtDlpMediaLocator;
use crate::process::{YtDlpProcessConfig, resolve_yt_dlp_candidate_document};

/// Извлекает public format inventory и нормализует immutable S19 snapshot.
pub fn resolve_yt_dlp_candidate_snapshot_with_config(
    locator: &YtDlpMediaLocator,
    source: SourceIdentity,
    generation: ExtractionGeneration,
    yt_dlp_config: &YtDlpConfig,
) -> Result<YtDlpCandidateSnapshot, YtDlpServiceError> {
    if !yt_dlp_config.enabled {
        return Err(YtDlpServiceError::AdapterDisabled);
    }
    let process_config = YtDlpProcessConfig::from_yt_dlp_config(yt_dlp_config)?;
    let document =
        resolve_yt_dlp_candidate_document(locator.expose_secret_for_open(), &process_config)?;
    Ok(normalize_candidate_document(document, source, generation))
}

#[cfg(test)]
mod tests;
